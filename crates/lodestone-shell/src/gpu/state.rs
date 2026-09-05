//! Building a [`RenderState`], and the install/setter seam every per-frame
//! source is wired through.
//!
//! Two kinds of thing live here and they degrade differently. The **installs**
//! ([`RenderState::install_sky`], `install_screen_effects`, `install_weather`,
//! `install_particle_sheet_atlas`) hand over a pass built by
//! [`crate::resources`], which owns the `client.jar` IO this module
//! deliberately has none of; never calling one draws nothing extra, which is
//! the honest degradation. The **sources** ([`super::sources`]) install a boxed
//! closure polled once per frame, because `RenderState` cannot reach `Sim` or
//! the network handle itself — several must be re-installed *every* frame
//! because their value is partial-tick interpolated, and each one's own doc
//! says so.
//!
//! Every `has_*` accessor next door exists for one reason, recorded once here
//! rather than nine times below: a wrong *value* and a missing *wiring* look
//! identical on screen, and only one of them is a bug in this module.
use std::collections::HashMap;

use lodestone_assets::ResourceLocation;
use lodestone_render::{
    BlockAtlas, BlockPipeline, Camera, CameraUniform, DepthBuffer, GpuAtlas, ItemVariants,
    ModelPipeline, SpriteAnimation,
    block::sprite_uv_buffer,
    crack_pipeline::CrackPipeline,
    crack_resolver::CrackResolver,
    fog::{FogSettings, FogUniform},
    model_anim_buffer, model_camera_buffer, model_shared_camera_buffer,
};

use glam::Vec3;

use crate::particles::{ParticleInstance, ParticleRenderer};

use super::distant_terrain::{DistantTerrainRenderer, HorizonGpuError};
use super::first_person;
use super::gpu_timing;
use super::terrain::{
    MODEL_ORIGIN_ARENA_SLOTS, ModelRenderer, PACKED_ORIGIN_ARENA_SLOTS, SectionOriginArena,
    anim_slots_at,
};
use super::{
    AmbientLightSource, BannerSource, BeaconBeamRenderer, BeaconSource, BellSource,
    EndGatewayBeamSource, EndGatewaySource, EndPortalRenderer, EndPortalSource,
    BlockEntityRenderer, BlockEntitySource, BrushableSource, CampfireSource, ConduitSource,
    CopperGolemStatueSource, ShelfSource,
    DEFAULT_RENDER_DISTANCE_CHUNKS, DecoratedPotSource, EnchantingTableSource, ShulkerSource,
    DebugLineRenderer, DebugLineVertex, DebugLinesSource, DisplayTextRenderer, EntityLightSource,
    EntityRenderer, HandSwingSource, ItemUseSource, ItemUseState, LecternSource, MainHandSource, MapSource,
    MovingPistonSource,
    NameTagRenderer, OutlineRenderer, OutlineShapeSource,
    PluginBillboardInstance, PluginBillboardRenderer, PluginBillboardsSource,
    RenderState, SKY_COLOR, ShadowGroundSource, SignSource, SignTextRenderer, SkullSource, SkyDarkenSource,
    SpawnerSource, ThirdPersonBodySource, ThirdPersonBodyState, TimeOfDaySource, VaultSource,
    transparent_placeholder_atlas,
};

impl RenderState {

    /// Build the pipeline and atlas for a target of `color_format` and size.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        vanilla: Option<&BlockAtlas>,
    ) -> Self {
        let pipeline = BlockPipeline::new(device, color_format);

        // The live world binds the real stitched vanilla atlas; the demo world
        // binds the procedural colour atlas. The two are disjoint id spaces, so
        // the choice is made once here and mirrors the mesh classifier.
        let (atlas, uv_buffer) = match vanilla {
            Some(va) => {
                let atlas = GpuAtlas::from_atlas_terrain(device, queue, va.atlas());
                let uv_buffer = sprite_uv_buffer(device, va.uv_table());
                (atlas, uv_buffer)
            }
            None => {
                let atlas_data = crate::blocks::build_atlas();
                let atlas = GpuAtlas::from_rgba(
                    device,
                    queue,
                    atlas_data.width,
                    atlas_data.height,
                    &atlas_data.rgba,
                    &atlas_data.sprite_rects,
                );
                let uv_buffer = sprite_uv_buffer(device, &atlas_data.uv_table);
                (atlas, uv_buffer)
            }
        };
        let atlas_bind_group = pipeline.atlas_bind_group(device, &atlas, &uv_buffer);
        // The packed path shares a camera and per-section origin arena. One bind
        // group covers both, built once here; every packed section draw reuses
        // it and varies only the dynamic offset.
        let packed_origin_arena = SectionOriginArena::new(
            device,
            queue,
            "lodestone-packed-section-origin-arena",
            PACKED_ORIGIN_ARENA_SLOTS,
        );
        let packed_shared_cam_buffer = lodestone_render::block::shared_camera_buffer(
            device,
            glam::Mat4::IDENTITY.to_cols_array_2d(),
            FogUniform::disabled(),
        );
        let packed_cam_bind_group = pipeline.camera_bind_group(
            device,
            &packed_shared_cam_buffer,
            packed_origin_arena.buffer(),
        );
        let depth = DepthBuffer::new(device, width.max(1), height.max(1));
        let outline = OutlineRenderer::new(device, color_format);
        let debug_lines = DebugLineRenderer::new(device, color_format);
        // A second instance of the same renderer for the fishing line — see
        // `RenderState::fishing_line`'s own doc for why it is not the same one.
        let fishing_line = DebugLineRenderer::new(device, color_format);
        // The billboard channel's own sprite table — the block atlas's
        // `ResourceLocation` → UV rect index `plugin_billboard_vertices`
        // resolves `PluginTexture::Named` against. Built once here, from the
        // same `atlas` this constructor just uploaded, and snapshotted into
        // an `Arc` so `install_plugin_billboards_source` (`app/session.rs`)
        // can clone it into a closure without borrowing `RenderState`. Empty
        // on the demo (no `vanilla` atlas) path — see `plugin_atlas_sprites`'
        // field doc.
        let plugin_atlas_sprites: HashMap<String, [f32; 4]> = vanilla.map_or_else(HashMap::new, |va| {
            va.atlas()
                .sprites()
                .iter()
                .map(|s| {
                    (
                        s.location.to_string(),
                        [s.uv_min[0], s.uv_min[1], s.uv_max[0], s.uv_max[1]],
                    )
                })
                .collect()
        });
        let plugin_billboards =
            PluginBillboardRenderer::new(device, color_format, &atlas.view, &atlas.sampler);
        let entities = EntityRenderer::new(device, queue, color_format);
        let nametag = NameTagRenderer::new(device, color_format);
        let sign_text = SignTextRenderer::new(device, color_format);
        let display_text = DisplayTextRenderer::new(device, color_format);
        let beacon_beam = BeaconBeamRenderer::new(device, queue, color_format);
        let lightning_bolt = super::lightning_bolt::LightningBoltRenderer::new(device, color_format);
        let end_portal = EndPortalRenderer::new(device, queue, color_format);

        // The live vanilla atlas carries baked model geometry; build the model
        // render pass over its *complete* atlas (whose UVs the baked quads index,
        // distinct from the cube atlas bound above). The demo path has no models,
        // so this stays `None` and terrain draws through the packed pipeline.
        let model = vanilla.and_then(BlockAtlas::models).map(|models| {
            let pipeline = ModelPipeline::new(device, color_format);
            let surface_pipeline = ModelPipeline::for_surface(device, color_format);
            let map_surface_pipeline = ModelPipeline::for_map_surface(device, color_format);
            // The three diagnostic variants below are only ever selected by a
            // `LODESTONE_MAP_*` switch. Which depth decisions the "no depth"
            // ones actually drop is resolved from the environment, so a run can
            // remove the comparison, the write or the polygon offset on its own
            // rather than all three together.
            let map_depth = super::maps::map_diagnostic_switches().depth;
            let map_surface_no_cull_pipeline = ModelPipeline::for_map_surface_diagnostic(
                device,
                color_format,
                false,
                lodestone_render::model_pipeline::MapDepthDiagnostic::PRODUCTION,
            );
            let map_surface_no_depth_pipeline =
                ModelPipeline::for_map_surface_diagnostic(device, color_format, true, map_depth);
            let map_surface_no_cull_no_depth_pipeline =
                ModelPipeline::for_map_surface_diagnostic(device, color_format, false, map_depth);
            let water_pipeline = ModelPipeline::for_fluid(device, color_format);
            // Translucent **block** geometry (stained glass, ice, the nether
            // portal swirl): the same MODEL_WGSL shader and palette as
            // `pipeline`, alpha-blended instead of cutout-discarded. Distinct
            // from `water_pipeline`, whose `FLUID_WGSL` shader has no palette
            // and always applies the water tint — wrong for a palette-tinted
            // translucent block.
            let translucent_pipeline =
                ModelPipeline::for_layer(device, color_format, lodestone_render::RenderLayer::Translucent);
            let atlas = GpuAtlas::from_atlas_terrain(device, queue, models.atlas());
            let atlas_bind_group = pipeline.atlas_bind_group(device, &atlas);
            let palette_buffer =
                lodestone_render::model_palette_buffer(device, models.tint_palette());
            let palette_bind_group = pipeline.palette_bind_group(device, &palette_buffer);
            // Snapshot the animated sprites' timelines (slot order) so the
            // per-slot uniform can be rebuilt from the live game tick each frame.
            let animations: Vec<(SpriteAnimation, f32)> = models
                .sprite_animations()
                .iter()
                .cloned()
                .zip(models.anim_frame_v().iter().copied())
                .collect();
            // Build the uniform (slot 0 static) at tick 0; rewritten each frame.
            // Two bind groups wrap the one buffer because the pipelines number
            // the animation group differently (model = 3, fluid = 2).
            let anim_buffer = model_anim_buffer(device, &anim_slots_at(&animations, 0));
            let anim_bind_group = pipeline.anim_bind_group(device, &anim_buffer);
            let water_anim_bind_group = water_pipeline.anim_bind_group(device, &anim_buffer);
            // Mining-crack overlay: capture the per-state quads + stage rects now,
            // while `models` is still borrowable, and build the pass's own atlas
            // and camera bind groups (its layouts differ from the model pass's).
            let crack_pipeline = CrackPipeline::new(device, color_format);
            let crack_resolver = CrackResolver::from_models(models);
            let crack_atlas_bind_group = crack_pipeline.atlas_bind_group(device, &atlas);
            let crack_cam_buffer = model_camera_buffer(
                device,
                CameraUniform {
                    view_proj: [[0.0; 4]; 4],
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
            );
            let crack_cam_bind_group = crack_pipeline.camera_bind_group(device, &crack_cam_buffer);
            // Dropped items: snapshot the baked item geometry while `models` is
            // still in scope. The pass's camera bind group is built below,
            // shared with every section (see `origin_arena`).
            //
            // `item_forms_iter`, not `items()`: the latter yields only the
            // *inventory* form, and this snapshot feeds the world, the hand and
            // every mob's hand as well. Taking the flattened one here is precisely
            // the bug the variant axis exists to fix.
            let items: HashMap<ResourceLocation, ItemVariants> = models
                .item_forms_iter()
                .map(|(id, variants)| (id.clone(), variants.clone()))
                .collect();
            // The shared per-frame half of the section camera (view_proj +
            // fog) and the per-section origin arena use one bind group, built
            // once; every section draw and the dropped-item pass reuse it,
            // varying only the dynamic offset.
            let origin_arena = SectionOriginArena::new(
                device,
                queue,
                "lodestone-model-section-origin-arena",
                MODEL_ORIGIN_ARENA_SLOTS,
            );
            let shared_cam_buffer = model_shared_camera_buffer(device, glam::Mat4::IDENTITY.to_cols_array_2d());
            let cam_bind_group =
                pipeline.camera_bind_group(device, &shared_cam_buffer, origin_arena.buffer());
            // The first-person held item's own group-0: `hand_projection` alone
            // (no world position), rewritten per frame from the live aspect
            // ratio. Its origin binding still points at the shared arena's
            // reserved zero slot.
            let hand_cam_buffer = model_shared_camera_buffer(device, [[0.0; 4]; 4]);
            let hand_cam_bind_group =
                pipeline.camera_bind_group(device, &hand_cam_buffer, origin_arena.buffer());
            ModelRenderer {
                pipeline,
                surface_pipeline,
                map_surface_pipeline,
                map_surface_no_cull_pipeline,
                map_surface_no_depth_pipeline,
                map_surface_no_cull_no_depth_pipeline,
                water_pipeline,
                translucent_pipeline,
                atlas,
                atlas_bind_group,
                palette_bind_group,
                palette_buffer,
                animations,
                anim_buffer,
                anim_bind_group,
                water_anim_bind_group,
                crack_pipeline,
                crack_resolver,
                crack_atlas_bind_group,
                crack_cam_buffer,
                crack_cam_bind_group,
                items,
                shared_cam_buffer,
                cam_bind_group,
                origin_arena,
                // Lazy: the first block is allocated on the first section upload,
                // so a run that never loads a world reserves no mesh VRAM.
                mesh_arena: lodestone_render::ModelMeshArena::new(),
                hand_cam_buffer,
                hand_cam_bind_group,
                sections: HashMap::new(),
                seen: std::collections::HashSet::new(),
            }
        });

        // Terrain debris samples the same atlas the terrain does. The two block
        // atlases are disjoint UV spaces (the packed cube atlas vs the complete
        // baked-model atlas), so binding the wrong one throws correctly-shaped
        // debris in some other block's colours.
        //
        // Sheet particles (flame, smoke, crits, splashes) come from a *third*
        // stitch, which `RenderState` cannot build for itself (it does no
        // `client.jar` IO, by design) — so it starts with a transparent
        // stand-in in the sheet slots and the shell calls
        // `install_particle_sheet_atlas` once it has the atlas. Until then a
        // sheet particle samples transparent black and is discarded on alpha:
        // it draws *nothing*, which is wrong but honest, instead of sampling
        // arbitrary block texels from an unrelated atlas.
        let particles = ParticleRenderer::new(device, color_format);
        let particle_atlas = model.as_ref().map_or(&atlas, |m| &m.atlas);
        let sheet_placeholder = transparent_placeholder_atlas(device, queue);
        let particle_atlas_bind_group = particles.atlas_bind_group(
            device,
            &particle_atlas.view,
            &particle_atlas.sampler,
            &sheet_placeholder.view,
            &sheet_placeholder.sampler,
        );

        Self {
            pipeline,
            atlas,
            uv_buffer,
            atlas_bind_group,
            depth,
            // An explicit horizon setting installs this later; ordinary chunk
            // rendering remains the default path and pays no distant-tier cost.
            distant_terrain: None,
            distant_terrain_failed: false,
            sections: HashMap::new(),
            packed_shared_cam_buffer,
            packed_cam_bind_group,
            packed_origin_arena,
            model,
            outline,
            debug_lines,
            debug_lines_source: DebugLinesSource::default(),
            fishing_line,
            plugin_billboards,
            plugin_billboards_source: PluginBillboardsSource::default(),
            plugin_atlas_sprites: std::sync::Arc::new(plugin_atlas_sprites),
            entities,
            instance_arena: lodestone_render::InstanceBufferArena::default(),
            flame_frame_counter: std::cell::Cell::new(0),
            section_fade_tick: std::cell::Cell::new(0),
            particles,
            particle_atlas_bind_group,
            particle_sheet_atlas: None,
            warned_missing_particle_sheet: false,
            // Full-bright until the shell installs a world sampler; see
            // `set_entity_light_source`.
            entity_light: EntityLightSource::default(),
            // No ground until the shell installs a world sampler; see
            // `set_shadow_ground_source`.
            shadow_ground: ShadowGroundSource::default(),
            // Vanilla's own default; see `set_entity_shadows_enabled`.
            entity_shadows_enabled: true,
            // Permanent noon until the shell installs a world clock; see
            // `set_sky_darken_source`.
            sky_darken: SkyDarkenSource::default(),
            // The overworld's own ambient colour until the shell installs the
            // current dimension; see `set_ambient_light_source`.
            ambient_light: AmbientLightSource::default(),
            // No third-person camera exists yet; see
            // `set_third_person_body_source`.
            third_person_body: ThirdPersonBodySource::default(),
            // A rested arm until the shell installs its tick-driven swing clock;
            // see `set_hand_swing_source`.
            hand_swing: HandSwingSource::default(),
            item_use: ItemUseSource::default(),
            // An empty hand until the shell installs a source; see
            // `set_main_hand_source`.
            main_hand: MainHandSource::default(),
            // Fully equipped and holding nothing: `Default` is the resting state, so
            // the first `set_main_hand_source` seeds rather than animating from a
            // dipped hand. See `HeldItemEquip::last`.
            equip: first_person::HeldItemEquip::default(),
            // Unbobbed until the shell installs a source; see `HandBobSource`.
            hand_bob: first_person::HandBobSource::default(),
            outline_shape: OutlineShapeSource::default(),
            // No sky until the shell installs one; see `install_sky`.
            sky: None,
            // Permanent noon until the shell installs a world clock; see
            // `set_time_of_day_source`.
            time_of_day: TimeOfDaySource::default(),
            // No underwater/fire overlay until the shell installs one; see
            // `install_screen_effects`.
            screen_effects: None,
            nametag,
            color_format,
            // Equal to `color_format` until the frame loop asks for the raw
            // world-text pass; see `set_world_text_view`.
            world_text_format: color_format,
            world_text_view: std::cell::RefCell::new(None),
            block_entities: BlockEntityRenderer::new(device, queue, color_format),
            // No chests until the shell installs a world source; see
            // `set_block_entity_source`.
            block_entity_source: BlockEntitySource::default(),
            skull_source: SkullSource::default(),
            // Likewise `set_copper_golem_statue_source`.
            copper_golem_statue_source: CopperGolemStatueSource::default(),
            // No bells until a caller installs a world source; see
            // `set_bell_source`, installed per frame by `app::redraw`.
            bell_source: BellSource::default(),
            // Likewise `set_spawner_source`.
            spawner_source: SpawnerSource::default(),
            // Likewise `set_shulker_source`.
            shulker_source: ShulkerSource::default(),
            // Likewise `set_decorated_pot_source`.
            decorated_pot_source: DecoratedPotSource::default(),
            // Likewise `set_conduit_source`.
            conduit_source: ConduitSource::default(),
            // Likewise `set_map_source`.
            map_source: MapSource::default(),
            map_cache: std::cell::RefCell::default(),
            // Likewise `set_banner_source`.
            banner_source: BannerSource::default(),
            // Likewise `set_lectern_source`.
            lectern_source: LecternSource::default(),
            // Likewise `set_campfire_source`.
            campfire_source: CampfireSource::default(),
            // Likewise `set_vault_source`.
            vault_source: VaultSource::default(),
            // Likewise `set_brushable_source`.
            brushable_source: BrushableSource::default(),
            // Likewise `set_shelf_source`.
            shelf_source: ShelfSource::default(),
            // Likewise `set_moving_piston_source`.
            moving_piston_source: MovingPistonSource::default(),
            // Likewise `set_enchanting_table_source`.
            enchanting_table_source: EnchantingTableSource::default(),
            // Identity until `app::redraw` installs this frame's `bobHurt`; see
            // `set_eye_bob_transform`.
            eye_bob: glam::Mat4::IDENTITY,
            // Vanilla's own default — see the field's doc for why `0.0` would be
            // the wrong "nothing installed yet" value.
            damage_tilt_strength: 1.0,
            // Vanilla's own shipped option values, which are also the constants
            // `glint_uniform` used to hand over itself — so a renderer nobody
            // pushes glint options into shimmers exactly as it did before.
            glint_speed: lodestone_render::glint::DEFAULT_SPEED,
            glint_strength: lodestone_render::glint::DEFAULT_STRENGTH,
            // `Fancy` — vanilla's own default, and what the pass hardcoded through
            // `SkyFrame::new` before the option had anywhere to enter.
            cloud_status: lodestone_render::CloudStatus::default(),
            // `Overworld` — what the pass drew unconditionally before the
            // dimension had anywhere to enter.
            sky_mode: lodestone_render::SkyMode::default(),
            void_fog: lodestone_render::fog::VoidFog::OVERWORLD,
            sign_text,
            // No signs until the shell installs a world source; see
            // `set_sign_source`.
            sign_source: SignSource::default(),
            display_text,
            // No display entities until `set_display_draws` installs this
            // frame's extract — see that method's doc.
            display_draws: Vec::new(),
            beacon_beam,
            lightning_bolt,
            // No beacon beams until the shell installs a world source; see
            // `set_beacon_source`.
            beacon_source: BeaconSource::default(),
            end_portal,
            // No end portals/gateways until the shell installs a world
            // source; see `set_end_portal_source`/`set_end_gateway_source`.
            end_portal_source: EndPortalSource::default(),
            end_gateway_source: EndGatewaySource::default(),
            end_gateway_beam_source: EndGatewayBeamSource::default(),
            end_portal_game_time: 0.0,
            // No rain/snow droplets until the shell installs the two environment
            // textures; see `install_weather`.
            weather: None,
            // No enchantment glint until the shell installs the glint sheet; see
            // `install_glint`.
            glint: None,
            // A calm sky blue, so terrain reads clearly against it.
            clear: wgpu::Color {
                r: SKY_COLOR[0] as f64,
                g: SKY_COLOR[1] as f64,
                b: SKY_COLOR[2] as f64,
                a: 1.0,
            },
            // Fog fades into that same sky colour. Sized for `Config`'s default
            // render distance, on vanilla's own span rather than a
            // fraction; both shell bring-up paths override it from the *real*
            // configured render distance via `set_fog` before the first frame
            // (`app.rs`'s `sky_fog`), and the per-frame reconciliation then
            // tracks dimension and submersion, so this value only ever reaches
            // a screen in a test that builds a `RenderState` and never calls
            // `set_fog`.
            fog: FogSettings::for_render_distance(SKY_COLOR, DEFAULT_RENDER_DISTANCE_CHUNKS),
            render_distance_chunks: DEFAULT_RENDER_DISTANCE_CHUNKS,
            // On by default: an off-by-default cull is an island, and this repo
            // has nine of those.
            terrain_culling: true,
            terrain_cull_diagnostics: std::cell::RefCell::new(
                super::terrain_cull_diagnostics::Probe::from_environment(),
            ),
            vis_graph: lodestone_render::VisibilityGraph::new(),
            occlusion: std::cell::RefCell::default(),
            last_camera_block_pos: std::cell::Cell::new(None),
            // Likewise on by default, for the same reason — and it is harmless
            // until something populates the graph: an empty graph produces no
            // reachable set, which is the pre-U3 cull exactly. See
            // `TerrainOcclusion` for the two weaker settings and when to reach
            // for them.
            occlusion_mode: super::TerrainOcclusion::On,
            // `None` whenever the device was not granted `Features::TIMESTAMP_QUERY`
            // — see `gpu_timing`'s module doc for why that check reads
            // `device.features()` rather than the adapter's advertised set.
            // Four segments; `gpu_timing`'s module doc carries the table of
            // what each covers. Two are single passes (`"world"` — one real
            // `wgpu` pass fusing terrain/entities/block entities/particles/
            // weather/outline/debug/nametags, reported as one number because
            // that is genuinely what it is — and `"first_person"`), and two
            // are spans bracketing whole command buffers (`"world_total"`,
            // `"hud_total"`) so that **no** GPU pass this shell submits is
            // unaccounted for. The order here fixes the query-set indices, so
            // adding a segment is append-only in spirit; nothing reads them
            // positionally, but a reordering silently rebases every index a
            // `stamp`/`writes` call resolves.
            gpu_timer: std::cell::RefCell::new(gpu_timing::GpuQueryTimer::new(
                device,
                queue,
                "lodestone-frame-gpu-timer",
                &["world_total", "world", "first_person", "hud_total"],
            )),
        }
    }

    /// Turn the per-frame terrain cull (distance ∩ frustum ∩ occlusion) on or
    /// off. On by default.
    ///
    /// This is vanilla's `smartCull` equivalent and the one-call false-cull
    /// diagnosis: if missing terrain reappears with culling off, a cull dropped
    /// it; if it does not, the section was never resident and the bug is
    /// upstream in streaming or meshing. It is also the A/B lever the
    /// draw-submission instruction harness measures both arms with
    /// (`tests/client_chunk_cycles.rs`).
    pub fn set_terrain_culling(&mut self, enabled: bool) {
        self.terrain_culling = enabled;
    }

    /// This session's GPU-timing capability: `false` when the device was not
    /// granted `Features::TIMESTAMP_QUERY` (see `gpu_timing`'s module doc) —
    /// a caller building the debug overlay/tracing line must show this
    /// explicitly rather than reporting the segments below as empty.
    #[must_use]
    pub fn gpu_timing_available(&self) -> bool {
        self.gpu_timer.borrow().is_some()
    }

    /// Per-segment GPU pass timings from the last completed readback —
    /// `(name, None)` for a segment with no reading yet (start-of-session
    /// latency, or a pass that has never run), `(name, Some(ms))` otherwise.
    /// Empty when [`Self::gpu_timing_available`] is `false`. See
    /// `gpu_timing::GpuQueryTimer::results_ms`'s doc for why a `None` here
    /// must never be rendered as `0.0`.
    #[must_use]
    pub fn gpu_timing_report(&self) -> Vec<(&'static str, Option<f32>)> {
        self.gpu_timer
            .borrow()
            .as_ref()
            .map(|t| t.results_ms().collect())
            .unwrap_or_default()
    }

    /// Frames where GPU-timing readback fell behind — see
    /// `gpu_timing::GpuQueryTimer::stalled_frames`'s doc. `0` whenever GPU
    /// timing is unavailable, same as every other counter here.
    #[must_use]
    pub fn gpu_timing_stalled_frames(&self) -> u64 {
        self.gpu_timer.borrow().as_ref().map_or(0, gpu_timing::GpuQueryTimer::stalled_frames)
    }

    /// Replace the distance-fog settings (colour + range) **and the sky disc's
    /// centre colour**, which travel together in [`FogSettings`]. The shell
    /// drives this from its configured render distance and the eye-in-fluid
    /// state: a sky-coloured fog sized to the render distance normally, a short
    /// biome-coloured water fog when submerged. Pass [`FogSettings::disabled`]
    /// to turn fog off.
    ///
    /// `FogSettings::sky_color` is what the sky pass paints the disc centre with
    /// (per-biome tint). It is in the same struct rather than behind
    /// its own setter precisely so a caller cannot update one and forget the
    /// other — see [`FogSettings`]' doc and
    /// [`set_clear_color`](Self::set_clear_color) below.
    /// `render_distance_chunks` rides along for the same reason `sky_color` is in
    /// the struct: the sky disc's gradient end is `min(render_distance, the
    /// attribute)`, so it is a *second*
    /// consumer of the same number the fog band already needs. The sky gradient
    /// endpoint is capped by this value, so keeping it explicit prevents the
    /// sky pass from falling back to its 512-block default and keeps both passes
    /// synchronized.
    /// Taking it as a parameter rather than adding a `set_render_distance` next
    /// door makes that unrepresentable: you cannot set fog without saying what
    /// distance it is for.
    pub fn set_fog(&mut self, fog: FogSettings, render_distance_chunks: u32) {
        self.fog = fog;
        self.render_distance_chunks = render_distance_chunks;
    }

    pub(crate) fn install_distant_terrain(
        &mut self,
        device: &wgpu::Device,
        camera_block: [i32; 2],
    ) -> Result<(), HorizonGpuError> {
        if self.distant_terrain_failed {
            return Ok(());
        }
        if self.distant_terrain.is_none() {
            match DistantTerrainRenderer::new(device, self.color_format, camera_block) {
                Ok(renderer) => self.distant_terrain = Some(renderer),
                Err(error) => {
                    self.distant_terrain_failed = true;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn disable_distant_terrain(&mut self) {
        self.distant_terrain = None;
        self.distant_terrain_failed = false;
    }

    pub(crate) fn recenter_distant_terrain(&mut self, camera_block: [i32; 2]) {
        if let Some(distant) = &mut self.distant_terrain {
            distant.recenter(camera_block);
        }
    }

    pub(crate) fn set_distant_terrain_near_field(&mut self, chunks: u32) {
        if let Some(distant) = &mut self.distant_terrain {
            distant.set_near_field_radius_chunks(chunks);
        }
    }

    pub(crate) fn set_distant_terrain_outer_radius(&mut self, chunks: u32) {
        if let Some(distant) = &mut self.distant_terrain {
            distant.set_outer_radius_chunks(chunks);
        }
    }

    pub(crate) fn populate_distant_terrain_one(
        &mut self,
        queue: &wgpu::Queue,
        sample: impl FnMut(i32, i32) -> lodestone_render::HorizonCell,
    ) -> bool {
        self.distant_terrain
            .as_mut()
            .is_some_and(|distant| distant.populate_one(queue, sample))
    }

    #[cfg(test)]
    pub(super) fn distant_terrain_rejects_unpopulated_submission(&self, slot: usize) -> bool {
        self.distant_terrain
            .as_ref()
            .is_some_and(|distant| distant.rejects_unpopulated_submission(slot))
    }

    /// The colour format the three flat-colour world-text passes must target,
    /// given the format the render target itself reports: its **raw**
    /// (non-sRGB) counterpart, which is the format itself whenever it is
    /// already raw.
    ///
    /// The whole decision, as a pure function, so a gate can assert it against
    /// a native-shaped format and a web-shaped one without standing up a
    /// surface — the same seam `lodestone_render::target`'s own
    /// `choose_view_format` gates are written against.
    #[must_use]
    pub fn gamma_text_format(color_format: wgpu::TextureFormat) -> wgpu::TextureFormat {
        color_format.remove_srgb_suffix()
    }

    /// The format the world-text pipelines are built for right now. Equal to
    /// the target's own format until [`Self::set_world_text_view`] is called;
    /// [`Self::gamma_text_format`] of it afterwards. Exists so a gate can
    /// assert the pair rather than discover a mismatch as a `wgpu` validation
    /// abort in whichever frame happens to carry a nametag.
    #[must_use]
    pub fn world_text_format(&self) -> wgpu::TextureFormat {
        self.world_text_format
    }

    /// Point the three flat-colour world-text passes — nametags, sign text and
    /// `text_display` panels and glyphs — at `frame`'s **raw** (non-sRGB) view
    /// of the very texture the world is already drawing into. Call once per
    /// frame, before [`Self::render`] and friends.
    ///
    /// # Why these three want a different view of the same pixels
    ///
    /// Vanilla is not colour-managed: `Font`'s glyphs and the plate behind them
    /// composite straight onto the framebuffer's gamma bytes. Every pipeline in
    /// this crate targets the swapchain's *sRGB* view, so the hardware decodes
    /// the destination, blends in linear light and re-encodes. For the plate
    /// (black at 25% alpha) that reads too weak against a bright backdrop and
    /// exactly right against black — re-derived from the sRGB transfer
    /// function, `0.75·bg` (vanilla) against `encode(0.75·decode(bg))` (here)
    /// is 0 at `bg = 0`, +7/255 at 64, +16/255 at 128 and +33/255 at 255.
    /// Black is the only fixed point; white is not, unlike the HUD tab-list
    /// case `docs/tab-list.md` records. The constants are vanilla's own and are
    /// **not** the bug, so there is nothing to tune: the geometry has to reach
    /// a raw view instead.
    ///
    /// All three move together deliberately. They share one shader
    /// (`shaders/nametag.wgsl` — flat vertex colour, no texture at all), so
    /// every colour any of them submits is a vanilla gamma byte: a sign's
    /// own ARGB-scale-RGB applied to `(dye, 0.4)`, a coloured nametag span, a `text_display`
    /// panel. Fixing one alone would leave the three visibly disagreeing.
    ///
    /// # What it costs, and when it is optional
    ///
    /// A `wgpu` render pass fixes one attachment format for every pipeline in
    /// it, so this is a *separate pass* rather than a different pipeline in the
    /// existing one — the whole-pipeline alternative is what the HUD's own fix
    /// tried and had to revert. `render_inner` opens those passes only when the
    /// frame actually has text to draw, so a scene with no signs, holograms or
    /// nametags pays nothing at all.
    ///
    /// On a target that is *already* non-sRGB this changes nothing and calling
    /// it is optional: every headless pixel gate uses `Rgba8Unorm`, and a
    /// browser canvas structurally has no sRGB format to be configured with
    /// (see `lodestone_render::target`'s `choose_view_format`), so in both the
    /// raw view and `RenderTarget::format`'s view are the same format and the
    /// blend was always on gamma bytes. **It is the native sRGB swapchain this
    /// exists for**, which is also why no headless gate can observe the
    /// difference.
    pub fn set_world_text_view(
        &mut self,
        device: &wgpu::Device,
        frame: &lodestone_render::AcquiredFrame,
    ) {
        let raw = Self::gamma_text_format(self.color_format);
        if self.world_text_format != raw {
            self.nametag.set_color_format(device, raw);
            self.sign_text.set_color_format(device, raw);
            self.display_text.set_color_format(device, raw);
            self.world_text_format = raw;
        }
        *self.world_text_view.borrow_mut() = Some(frame.create_view(raw));
    }

    /// Replace the frame's clear colour — the colour drawn where nothing else
    /// covers a pixel (above the world, or beyond the far plane if a caller
    /// ever set one shorter than the horizon).
    ///
    /// Mirrors [`set_fog`](Self::set_fog) exactly, and is meant to be called
    /// with the *same* `[f32; 3]` as the fog colour just set
    /// (`docs/dimension-visuals.md`'s wiring note): [`SKY_COLOR`]'s own doc
    /// comment records that a second, independently-maintained copy of the sky
    /// colour is exactly how the horizon has previously ended up banding in a
    /// colour the sky never actually is. There is deliberately no
    /// dimension-aware default baked in here — the caller (`app.rs`) already
    /// computed the right colour for `set_fog`; this just stops the clear from
    /// disagreeing with it.
    pub fn set_clear_color(&mut self, color: [f32; 3]) {
        self.clear = wgpu::Color {
            r: f64::from(color[0]),
            g: f64::from(color[1]),
            b: f64::from(color[2]),
            a: 1.0,
        };
    }

    /// Like [`set_clear_color`](Self::set_clear_color), but applies the same
    /// `FOG_COLOR` day/night track [`fog_with_clock`](Self::fog_with_clock)
    /// applies to the fogged passes, deriving it from `self.time_of_day`
    /// exactly the same way. `day_base` is the *untracked* day colour — pass
    /// the same value that goes into [`set_fog`](Self::set_fog), never a
    /// pre-tracked one (see `fog_with_clock`'s doc on why pre-multiplying the
    /// stored base double-applies the track elsewhere).
    ///
    /// This clear only paints anything when [`has_sky`](Self::has_sky) is
    /// `false`: with a sky installed, the sky pass overwrites it with its own
    /// tracked colour every frame (`gpu.rs`'s sky-frame call site). Without
    /// one — a jar-less run — the untracked clear was a bright sky-blue void
    /// at night; this is that loose end, closed.
    pub fn set_clear_color_tracked(&mut self, day_base: [f32; 3]) {
        self.set_clear_color(Self::clear_color_tracked_for(
            self.time_of_day.value(),
            day_base,
        ));
    }

    /// Pure core of [`set_clear_color_tracked`](Self::set_clear_color_tracked),
    /// split out for the same reason [`fog_uniform_for`](Self::fog_uniform_for)
    /// is: testable without a GPU device.
    pub(super) fn clear_color_tracked_for(time_of_day: i64, day_base: [f32; 3]) -> [f32; 3] {
        lodestone_render::fog_color_for_time_of_day(time_of_day, day_base)
    }

    /// Install the world light sampler mobs are lit by (see
    /// [`EntityLightSource`]). Call once, after a world exists; without it every
    /// mob renders [`ENTITY_FULLBRIGHT`] and out-shines the terrain it stands
    /// in — a mob in a cave or a shadow stays bright.
    ///
    /// **This alone does not fix night.** An earlier version of this doc claimed
    /// it did. The server's sky-light array is time-invariant, so a mob standing
    /// under open sky samples `0xF0` at midnight exactly as at noon; darkening
    /// with the clock is [`set_sky_darken_source`](Self::set_sky_darken_source)'s
    /// job and nothing this sampler returns can substitute for it.
    ///
    /// `f` receives an entity's **feet** position and returns its packed
    /// `sky << 4 | block` light, or `None` outside loaded chunks. The equivalent
    /// world lookup already exists for particles in `Sim::extract_particles`.
    pub fn set_entity_light_source(
        &mut self,
        f: impl Fn(Vec3) -> Option<u8> + Send + Sync + 'static,
    ) {
        self.entity_light = EntityLightSource(Some(Box::new(f)));
    }

    /// Install the world block sampler the entity-shadow pass scans the
    /// ground with (see [`ShadowGroundSource`]). Call once, after a world
    /// exists; without it no entity ever casts a shadow, the same "unset
    /// reproduces the pre-feature behaviour" every other polled source here
    /// documents.
    ///
    /// `f` receives a world block position and returns the raw block-state id
    /// there, or `None` outside loaded chunks — exactly [`NetClient::block_at`]'s
    /// own shape.
    ///
    /// [`NetClient::block_at`]: crate::net::NetClient::block_at
    pub fn set_shadow_ground_source(
        &mut self,
        f: impl Fn([i32; 3]) -> Option<u32> + Send + Sync + 'static,
    ) {
        self.shadow_ground = ShadowGroundSource(Some(Box::new(f)));
    }

    /// Push vanilla's `entityShadows` video option down into the renderer.
    /// Cheap to call every frame — a plain bool write — so the shell's usual
    /// per-frame options sync (`app/redraw.rs`, beside
    /// `Sim::set_cutout_leaves`) can poll it exactly like every other live
    /// option rather than firing it only on toggle.
    pub fn set_entity_shadows_enabled(&mut self, enabled: bool) {
        self.entity_shadows_enabled = enabled;
    }

    /// This frame's fog uniform with the sky-darken factor folded into its spare
    /// lane, so **terrain and mobs read the same clock**. Wiring one without the
    /// other is worse than wiring neither: at midnight it makes mobs darker than
    /// the blocks they stand on, which reads as a mob-rendering bug rather than a
    /// missing feature.
    ///
    /// Also folds in the `FOG_COLOR` day/night track
    /// (`lodestone_render::fog_color_for_time_of_day`), which until this existed
    /// only reached the sky disc (`sky_pipeline.rs`) — terrain and entity fog
    /// rendered a full-brightness day colour at any hour, so at midnight distant
    /// chunks faded to a bright sky blue against a near-black sky. **This is the
    /// only place that may apply the track**: `self.fog.color` must stay the
    /// untracked day base, because the sky pass (`gpu.rs`'s sky-frame call site)
    /// reads `self.fog.color` directly and applies the same track itself to
    /// paint the disc's horizon. Pre-multiplying the stored base here or in
    /// `set_fog` would double-apply the track to the sky disc.
    pub(super) fn fog_with_clock(&self, eye: glam::Vec3) -> FogUniform {
        Self::fog_uniform_for(
            &self.fog,
            self.time_of_day.value(),
            self.sky_darken.value(),
            self.ambient_light.value(),
            [eye.x, eye.y, eye.z],
            // The section fade-in's clock (`lodestone_render::
            // section_visibility`), in the same seconds a section's
            // `build_time` uses — see `Self::section_fade_tick`'s doc.
            // `TICK_PERIOD` (1/20 s) rather than a wall clock: this crate's
            // render path must never call `std::time::Instant`/`SystemTime`
            // (they trap on wasm32 — see `DESIGN.md`), and the live game tick
            // is already a portable clock threaded in via `update_animation`.
            self.section_fade_tick.get() as f32 / 20.0,
        )
    }

    /// Pure core of [`fog_with_clock`](Self::fog_with_clock), taking the
    /// frame's sourced values as plain arguments rather than reading `self`, so
    /// it is testable without a GPU device — `RenderState::new` requires one,
    /// and this crate's GPU gates are `#[ignore]`d, so a hermetic test of this
    /// logic needs a path that never constructs a `RenderState`.
    pub(super) fn fog_uniform_for(
        fog: &FogSettings,
        time_of_day: i64,
        sky_darken: f32,
        ambient_light: [f32; 3],
        eye: [f32; 3],
        now_secs: f32,
    ) -> FogUniform {
        let mut settings = *fog;
        settings.color = lodestone_render::fog_color_for_time_of_day(time_of_day, fog.color);
        let mut u = FogUniform::new(&settings, eye);
        u.end_enabled[2] = sky_darken;
        u.ambient_light = [ambient_light[0], ambient_light[1], ambient_light[2], now_secs];
        u
    }

    /// Install the world clock mobs are darkened by at night (see
    /// [`SkyDarkenSource`]). Install once, at connect time, next to
    /// [`set_entity_light_source`](Self::set_entity_light_source) — `f` is polled
    /// once per frame and may return `None` until the world clock is known, so
    /// there is nothing to wait for.
    ///
    /// `f` returns the factor the **sky** half of the lightmap is scaled by.
    /// Build it from the server's `time_of_day` with
    /// [`sky_darken_for_time_of_day`], which is vanilla's curve:
    ///
    /// ```no_run
    /// # use std::sync::{Arc, OnceLock};
    /// # fn wire(render: &mut lodestone::gpu::RenderState, net: &lodestone::net::NetClient) {
    /// use lodestone_render::entity::sky_darken_for_time_of_day;
    ///
    /// let clock = net.shared_handle();
    /// render.set_sky_darken_source(move || {
    ///     clock
    ///         .get()
    ///         .map(|h| sky_darken_for_time_of_day(h.world_time().1))
    /// });
    /// # }
    /// ```
    ///
    /// Without this, mobs render at permanent noon: the reported
    /// "mobs are still super bright, even at night".
    pub fn set_sky_darken_source(&mut self, f: impl Fn() -> Option<f32> + Send + Sync + 'static) {
        self.sky_darken = SkyDarkenSource(Some(Box::new(f)));
    }

    /// This frame's sky-darken factor, as the entity pass will use it. Exposed so
    /// the shell can surface it on the debug overlay: a wrong *value* and a
    /// missing *wiring* look identical on screen, and only one of them is a bug
    /// in this file.
    #[must_use]
    pub fn sky_darken(&self) -> f32 {
        self.sky_darken.value()
    }

    /// Install the current dimension's ambient light colour (see
    /// [`AmbientLightSource`]). Install once, at connect time, next to
    /// [`set_sky_darken_source`](Self::set_sky_darken_source) — `f` is polled
    /// once per frame and may return `None` until the dimension type is known,
    /// so there is nothing to wait for.
    ///
    /// `f` returns `EnvironmentAttributes.AMBIENT_LIGHT_COLOR` for the
    /// dimension the local player is currently in, as `0.0..=1.0` per-channel
    /// floats — `lodestone_render::light::rgb24_to_channels` of
    /// `DimensionType::ambient_light_color`:
    ///
    /// ```no_run
    /// # use std::sync::{Arc, OnceLock};
    /// # fn wire(render: &mut lodestone::gpu::RenderState, net: &lodestone::net::NetClient) {
    /// use lodestone_render::light::{OVERWORLD_AMBIENT_LIGHT, rgb24_to_channels};
    ///
    /// let handle = net.shared_handle();
    /// render.set_ambient_light_source(move || {
    ///     let dim = handle.get()?.player().dimension_type?;
    ///     Some(match dim.ambient_light_color {
    ///         Some(packed) => rgb24_to_channels(packed),
    ///         None => OVERWORLD_AMBIENT_LIGHT,
    ///     })
    /// });
    /// # }
    /// ```
    ///
    /// Without this, every dimension renders the overworld's own grey
    /// ambient floor, while the Nether's floor (`0x302821`) is markedly brighter
    /// than the overworld's (`#0a0a0a`).
    pub fn set_ambient_light_source(
        &mut self,
        f: impl Fn() -> Option<[f32; 3]> + Send + Sync + 'static,
    ) {
        self.ambient_light = AmbientLightSource(Some(Box::new(f)));
    }

    /// Install the sky pass, built once by the caller (typically
    /// `crate::resources::load_sky`, which owns the `client.jar` IO this file
    /// deliberately has none of). `None` — no call, a jar-less run — leaves
    /// [`render_inner`](Self::render_inner) exactly as it behaved before the
    /// sky existed: no sky pass runs, and the block pass clears straight to
    /// [`Self::clear`].
    pub fn install_sky(&mut self, sky: lodestone_render::SkyRenderer) {
        self.sky = Some(sky);
    }

    /// Whether a sky pass is installed. Exposed for the same reason
    /// [`sky_darken`](Self::sky_darken) is: a wrong *value* and a missing
    /// *wiring* must not look identical from outside this module.
    #[must_use]
    pub fn has_sky(&self) -> bool {
        self.sky.is_some()
    }

    /// Install the underwater/fire screen-overlay pass, built once by the
    /// caller (typically `crate::resources::load_screen_effects`, which owns
    /// the `client.jar` IO this file deliberately has none of — the same
    /// split [`install_sky`](Self::install_sky) uses). `None` — no call, a
    /// jar-less run — leaves [`render_inner`](Self::render_inner) drawing
    /// neither overlay, whatever [`ScreenEffects`] it is handed.
    pub fn install_screen_effects(&mut self, fx: lodestone_render::ScreenEffectRenderer) {
        self.screen_effects = Some(fx);
    }

    /// Whether the screen-overlay pass is installed. Same reason as
    /// [`has_sky`](Self::has_sky): a wrong *value* and a missing *wiring* must
    /// not look identical from outside this module.
    #[must_use]
    pub fn has_screen_effects(&self) -> bool {
        self.screen_effects.is_some()
    }

    /// Install the rain/snow pass from the two already-decoded environment
    /// textures — same caller/IO split as [`install_sky`](Self::install_sky).
    ///
    /// `depth_format` is taken from this state's own depth buffer rather than
    /// asked for: the pass draws inside the existing block pass, so a mismatch
    /// would be a wgpu validation error at draw time and there is exactly one
    /// right answer.
    pub fn install_weather(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        textures: &lodestone_render::WeatherTextures,
    ) {
        self.weather = Some(lodestone_render::WeatherRenderer::new(
            device,
            queue,
            color_format,
            lodestone_render::DEPTH_FORMAT,
            textures,
        ));
    }

    /// Whether the rain/snow pass is installed. Same reason as
    /// [`has_sky`](Self::has_sky).
    ///
    /// A `false` here with a non-zero rain level is a *jar* problem, not a wiring
    /// problem — the darkening still reaches pixels. `weather_columns() > 0` with
    /// this `false` is the state that draws nothing while claiming to.
    #[must_use]
    pub fn has_weather(&self) -> bool {
        self.weather.is_some()
    }

    /// Install the enchantment-glint pass from the already-decoded glint sheet
    /// — same caller/IO split as [`install_sky`](Self::install_sky):
    /// `crate::resources::load_glint_texture` owns the `client.jar` read, this
    /// owns the upload and pipeline build.
    ///
    /// The sheet is uploaded as **`Rgba8Unorm`**, not `_Srgb` — see
    /// `glint::GlintPass`'s module doc for why the glint is the one texture in
    /// this crate that must *not* be colour-decoded on the way in.
    pub fn install_glint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        img: &lodestone_assets::Image,
    ) {
        self.glint = Some(super::glint::GlintPass::new(device, queue, color_format, img));
    }

    /// Whether the glint pass is installed. Same reason as
    /// [`has_sky`](Self::has_sky): a wrong *value* and a missing *wiring* must
    /// not look identical from outside this module.
    #[must_use]
    pub fn has_glint(&self) -> bool {
        self.glint.is_some()
    }

    /// Write the shared glint group-0 uniform for one glint draw this frame.
    ///
    /// `view_proj` is whatever the *base* item pass just drew through — the
    /// depth-`EQUAL` contract requires the glint pass to rasterise byte-identical
    /// clip positions, so this value is not a second projection but the first
    /// one, handed back by the site that wrote it. No-op with no pass installed
    /// (a jar-less run, or before [`install_glint`](Self::install_glint)), so a
    /// caller need not branch.
    pub(super) fn write_glint_uniform(&self, queue: &wgpu::Queue, view_proj: [[f32; 4]; 4]) {
        if let Some(glint) = &self.glint {
            queue.write_buffer(
                &glint.uniform_buffer,
                0,
                bytemuck::bytes_of(&super::glint::glint_uniform(
                    view_proj,
                    self.glint_speed,
                    self.glint_strength,
                    self.glint_atlas_px(),
                )),
            );
        }
    }

    /// The dimensions of the atlas a glint draw's vertices carry UVs into.
    ///
    /// Both glint buffers here shimmer item geometry, which is baked against the
    /// stitched **model** atlas — the same object
    /// `self.model.as_ref().map_or(&self.atlas, |m| &m.atlas)` resolves for every
    /// other consumer of it. It is not a constant: the stitcher's gutter is
    /// `1 << mipmapLevels`, so the sheet is repacked (and every UV with it)
    /// whenever that video setting moves, and vanilla's glint scale is expressed
    /// in atlas-normalised units. See `lodestone_render::glint::atlas_correction`.
    fn glint_atlas_px(&self) -> [u32; 2] {
        let atlas = self.model.as_ref().map_or(&self.atlas, |m| &m.atlas);
        [atlas.width, atlas.height]
    }

    /// Push vanilla's **Glint Speed**/**Glint Strength** accessibility options
    /// down from the menu layer, exactly as
    /// [`Self::set_damage_tilt_strength`] does for Damage Tilt — per frame,
    /// because the sliders live on a settings page and must move the shimmer
    /// while that page is still up.
    ///
    /// Both are `UnitDouble`s in `[0, 1]`; clamping lives in
    /// `gpu::glint::glint_uniform`, the one place both the world and hand draws
    /// pass through, so this setter cannot become a second copy of the domain.
    pub fn set_glint_options(&mut self, speed: f64, strength: f32) {
        self.glint_speed = speed;
        self.glint_strength = strength;
    }

    /// This frame's glint speed and strength, as they will reach the uniform —
    /// already clamped, so a gate can predict the value the GPU sees rather than
    /// the value that was pushed.
    #[must_use]
    pub fn glint_options(&self) -> (f64, f32) {
        (
            lodestone_render::glint::clamp_speed(self.glint_speed),
            lodestone_render::glint::clamp_strength(self.glint_strength),
        )
    }

    /// Push vanilla's **Clouds** option down from the menu layer. Read by the sky
    /// pass in `gpu/frame.rs` when it builds this frame's
    /// `lodestone_render::SkyFrame`.
    ///
    /// Per frame like every other option setter here, and cheap: the value only
    /// reaches a `SkyFrame` builder, so switching modes costs one comparison in
    /// `SkyRenderer::render` and no pipeline rebuild.
    pub fn set_cloud_status(&mut self, status: lodestone_render::CloudStatus) {
        self.cloud_status = status;
    }

    /// The cloud mode this frame's sky pass will draw. Exposed so a gate can assert
    /// the pushed value without a GPU adapter.
    #[must_use]
    pub fn cloud_status(&self) -> lodestone_render::CloudStatus {
        self.cloud_status
    }

    /// Push the connected **dimension's** `Skybox` down — vanilla's
    /// own per-dimension skybox choice. Read by the sky pass in `gpu/frame.rs` when it
    /// builds this frame's `lodestone_render::SkyFrame`.
    ///
    /// Deliberately a sibling of [`Self::set_cloud_status`] and **not** part of
    /// [`Self::set_fog`]: fog and the frame clear carry colours, which is why the
    /// Nether already had the right red horizon while the sun, moon, stars and
    /// clouds still drew over it. See [`lodestone_render::SkyMode`].
    pub fn set_sky_mode(&mut self, mode: lodestone_render::SkyMode) {
        self.sky_mode = mode;
    }

    /// The sky mode this frame's sky pass will draw. Exposed so a gate can assert
    /// the pushed value without a GPU adapter, exactly like
    /// [`Self::cloud_status`].
    #[must_use]
    pub fn sky_mode(&self) -> lodestone_render::SkyMode {
        self.sky_mode
    }

    /// Push this level's void-fog geometry down — the dimension's own `min_y`
    /// and vanilla's own void-darkness-onset-range's flat/non-flat fork.
    /// See [`crate::gpu::RenderState::void_fog`]'s field doc for what the two
    /// constants it replaced got wrong.
    pub fn set_void_fog(&mut self, void_fog: lodestone_render::fog::VoidFog) {
        self.void_fog = void_fog;
    }

    /// The void fog this frame's sky pass will use. Exposed for the same reason
    /// [`Self::sky_mode`] is: a gate can assert the pushed value with no GPU
    /// adapter at all.
    #[must_use]
    pub fn void_fog(&self) -> lodestone_render::fog::VoidFog {
        self.void_fog
    }

    /// [`Self::write_glint_uniform`] for the **world** glint draw — enchanted
    /// dropped items and enchanted items in mobs' hands, which rasterise in the
    /// main pass rather than the hand's own. Its own buffer for the reason
    /// `GlintPass::world_uniform_buffer` records.
    pub(super) fn write_world_glint_uniform(
        &self,
        queue: &wgpu::Queue,
        view_proj: [[f32; 4]; 4],
    ) {
        if let Some(glint) = &self.glint {
            queue.write_buffer(
                &glint.world_uniform_buffer,
                0,
                bytemuck::bytes_of(&super::glint::glint_uniform(
                    view_proj,
                    self.glint_speed,
                    self.glint_strength,
                    self.glint_atlas_px(),
                )),
            );
        }
    }

    /// Upload this frame's precipitation columns. Must run **before**
    /// [`render`](Self::render), like [`prepare_particles`](Self::prepare_particles)
    /// and for the same reason: buffers cannot be created mid-pass.
    ///
    /// `instances` must come from [`lodestone_render::column_instance`] over a
    /// [`lodestone_render::extract_columns`] result, and `rain_count` from
    /// [`lodestone_render::rain_count`] over that same (rain-first-sorted) list.
    /// The two travel together because the sort order is what makes the split
    /// meaningful; passing a count from a differently-ordered list textures snow
    /// as rain.
    ///
    /// A no-op with no pass installed, so a jar-less caller need not branch.
    pub fn prepare_weather(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[lodestone_render::WeatherInstance],
        rain_count: usize,
        camera: &Camera,
    ) {
        if let Some(weather) = &mut self.weather {
            weather.prepare(device, queue, instances, rain_count, camera);
        }
    }

    /// Columns uploaded by the last [`prepare_weather`](Self::prepare_weather),
    /// i.e. what the next frame will submit. `0` with no pass installed.
    ///
    /// Exposed for the same reason [`sky_darken`](Self::sky_darken) is: "the rain
    /// is not drawing" has two causes — no columns extracted, or columns extracted
    /// into no pass — and they look identical on screen.
    #[must_use]
    pub fn weather_columns(&self) -> usize {
        self.weather.as_ref().map_or(0, |w| w.count())
    }

    /// Of [`weather_columns`](Self::weather_columns), how many are rain rather
    /// than snow. `0` with no pass installed.
    ///
    /// Worth surfacing separately: with the biome-climate registry lane still
    /// missing (see [`lodestone_render::WeatherProbe::precipitation`]) this is
    /// currently *always* equal to `weather_columns`, and the day it stops being
    /// is the day snow started working.
    #[must_use]
    pub fn weather_rain_columns(&self) -> usize {
        self.weather.as_ref().map_or(0, |w| w.rain_count())
    }

    /// Upload the stitched particle sheet and rebind the particle pass to it.
    ///
    /// `atlas` **must** be the very same [`ParticleAtlas`] whose UV table was
    /// installed into [`crate::particles::Particles`] via
    /// `with_particle_atlas` — not a second stitch of the same pack. Two
    /// `AtlasBuilder` runs over one pack are byte-identical *today* (the
    /// definition paths are sorted and deduplicated), but that is a property of
    /// the packer, not a guarantee the type system holds, and the failure mode
    /// if it ever changes is precisely the bug this method exists to fix: UVs
    /// that resolve against a different packing. `app.rs` therefore hands the
    /// same `Arc` to both sides.
    ///
    /// Same install shape and reason as [`install_sky`](Self::install_sky): the
    /// `client.jar` IO lives in [`crate::resources`], and this module does
    /// none. Never called — a jar-less run — leaves the 1×1 transparent
    /// stand-in [`new`](Self::new) bound, so a sheet particle draws nothing
    /// rather than a block texel.
    pub fn install_particle_sheet_atlas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: &lodestone_assets::Atlas,
    ) {
        let sheet = GpuAtlas::from_atlas(device, queue, atlas);
        // Re-derived rather than remembered: the block half of this bind group
        // is whichever atlas the terrain pass draws from, and that choice is
        // already expressed once in `new`. Restating it here keeps the two from
        // drifting apart — a particle textured from an atlas the terrain does
        // not draw from is the same class of bug one layer over.
        let bind_group = {
            let block = self.model.as_ref().map_or(&self.atlas, |m| &m.atlas);
            self.particles.atlas_bind_group(
                device,
                &block.view,
                &block.sampler,
                &sheet.view,
                &sheet.sampler,
            )
        };
        tracing::info!(
            target: "assets",
            width = sheet.width,
            height = sheet.height,
            "bound the stitched particle sheet to the particle pass"
        );
        self.particle_sheet_atlas = Some(sheet);
        self.particle_atlas_bind_group = bind_group;
    }

    /// Re-upload `vanilla`'s packed cube atlas and (if present) its complete
    /// baked-model atlas, and rebind every pass that samples either — the
    /// GPU-side half of a live resource-pack reload
    /// (`crate::sim::Sim::reload_resource_pack_atlas` is the mesh/classifier
    /// half; call this with the atlas it returns).
    ///
    /// # Replaces bind groups in place, never adds a fifth
    ///
    /// The model shader is at wgpu's 4-bind-group floor (camera / atlas /
    /// palette / anim — see `CLAUDE.md`'s rendering-constraints section), so
    /// this must never grow the pipeline layout. It does not: every pipeline
    /// (`self.pipeline`, and `model.pipeline`/`water_pipeline`/
    /// `crack_pipeline` inside [`ModelRenderer`]) is untouched — only the
    /// *contents* of the atlas/palette/anim bind groups change, following the
    /// exact "rebuild the GPU object, overwrite the field" shape
    /// [`Self::install_particle_sheet_atlas`] already established for the
    /// particle sheet.
    ///
    /// # Why every already-uploaded section is dropped, not kept
    ///
    /// A fresh atlas re-packs every sprite at new coordinates, so a section
    /// meshed against the *old* atlas would sample the *new* one at the
    /// wrong UVs — wrong texels, not missing ones, which is a worse defect
    /// than a brief blank frame. [`ModelRenderer::sections`] is cleared here
    /// so nothing draws with stale UVs; the caller's forced remesh (already
    /// done by the time this runs — see
    /// `Sim::reload_resource_pack_atlas`'s doc) is what repopulates it over
    /// the next few frames as the mesh workers catch up.
    ///
    /// # What this deliberately does not reach
    ///
    /// The particle **sheet** half of [`Self::particle_atlas_bind_group`]
    /// (flame/smoke/crit sprites) is rebound here to keep pairing with the
    /// *new* block atlas, but its own pixels are not re-stitched — that is
    /// [`Self::install_particle_sheet_atlas`]'s job, and calling it separately
    /// needs `Particles`' own UV table rebuilt in the same step; that UV-table
    /// state belongs to the session (`Sim`) and is outside this module.
    /// Entity textures, the item atlas and the GUI/menu atlases are separate
    /// owners entirely — see `crate::app::lifecycle` for those.
    pub fn reload_block_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, vanilla: &BlockAtlas) {
        let new_atlas = GpuAtlas::from_atlas_terrain(device, queue, vanilla.atlas());
        let new_uv_buffer = sprite_uv_buffer(device, vanilla.uv_table());
        let new_atlas_bind_group = self
            .pipeline
            .atlas_bind_group(device, &new_atlas, &new_uv_buffer);
        self.atlas = new_atlas;
        self.uv_buffer = new_uv_buffer;
        self.atlas_bind_group = new_atlas_bind_group;

        match (vanilla.models(), self.model.as_mut()) {
            (Some(models), Some(model)) => {
                let new_model_atlas = GpuAtlas::from_atlas_terrain(device, queue, models.atlas());
                let new_model_atlas_bind_group =
                    model.pipeline.atlas_bind_group(device, &new_model_atlas);
                let new_crack_atlas_bind_group =
                    model.crack_pipeline.atlas_bind_group(device, &new_model_atlas);
                let new_palette_buffer =
                    lodestone_render::model_palette_buffer(device, models.tint_palette());
                let new_palette_bind_group =
                    model.pipeline.palette_bind_group(device, &new_palette_buffer);
                let new_animations: Vec<(SpriteAnimation, f32)> = models
                    .sprite_animations()
                    .iter()
                    .cloned()
                    .zip(models.anim_frame_v().iter().copied())
                    .collect();
                let new_anim_buffer = model_anim_buffer(device, &anim_slots_at(&new_animations, 0));
                let new_anim_bind_group = model.pipeline.anim_bind_group(device, &new_anim_buffer);
                let new_water_anim_bind_group =
                    model.water_pipeline.anim_bind_group(device, &new_anim_buffer);
                let new_crack_resolver = CrackResolver::from_models(models);
                let new_items: HashMap<ResourceLocation, ItemVariants> = models
                    .item_forms_iter()
                    .map(|(id, variants)| (id.clone(), variants.clone()))
                    .collect();

                model.atlas = new_model_atlas;
                model.atlas_bind_group = new_model_atlas_bind_group;
                model.crack_atlas_bind_group = new_crack_atlas_bind_group;
                model.palette_buffer = new_palette_buffer;
                model.palette_bind_group = new_palette_bind_group;
                model.animations = new_animations;
                model.anim_buffer = new_anim_buffer;
                model.anim_bind_group = new_anim_bind_group;
                model.water_anim_bind_group = new_water_anim_bind_group;
                model.crack_resolver = new_crack_resolver;
                model.items = new_items;
                model.sections.clear();
            }
            (Some(_), None) => {
                tracing::warn!(
                    target: "assets",
                    "resource pack reload found baked models but this session's \
                     RenderState has no ModelRenderer to reload — this should be \
                     unreachable (a live session's Sim gates on vanilla_atlas \
                     already being Some, which only ever pairs with a ModelRenderer)"
                );
            }
            (None, _) => {}
        }

        // Rebind the particle-debris half so it samples the *new* block
        // atlas rather than the one just dropped — see this method's own doc
        // for why the sheet half is a separate, un-reached surface.
        let particle_block_atlas = self.model.as_ref().map_or(&self.atlas, |m| &m.atlas);
        let regenerated_placeholder;
        let (sheet_view, sheet_sampler) = match &self.particle_sheet_atlas {
            Some(sheet) => (&sheet.view, &sheet.sampler),
            None => {
                regenerated_placeholder = transparent_placeholder_atlas(device, queue);
                (&regenerated_placeholder.view, &regenerated_placeholder.sampler)
            }
        };
        self.particle_atlas_bind_group = self.particles.atlas_bind_group(
            device,
            &particle_block_atlas.view,
            &particle_block_atlas.sampler,
            sheet_view,
            sheet_sampler,
        );

        tracing::info!(
            target: "assets",
            sprites = vanilla.sprite_count(),
            "reloaded the live block atlas from the currently selected resource packs"
        );
    }

    /// Rebind static block-entity sheets after a resource-pack generation bump.
    /// This is separate from [`Self::reload_block_atlas`] because player-head
    /// fallback sheets are not part of the block atlas.
    pub fn reload_block_entity_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.block_entities.reload_textures(device, queue);
    }

    /// Whether the stitched particle sheet is uploaded and bound. Same reason
    /// as [`has_sky`](Self::has_sky), with more teeth: with this `false` every
    /// flame, smoke and crit particle resolves, uploads, submits a draw — and
    /// discards on alpha. `particles_drawn` counts them all.
    #[must_use]
    pub fn has_particle_sheet_atlas(&self) -> bool {
        self.particle_sheet_atlas.is_some()
    }

    /// Install the world clock the sky pass reads (see [`TimeOfDaySource`]).
    /// Install once, at connect time, next to
    /// [`set_sky_darken_source`](Self::set_sky_darken_source) — `f` is polled
    /// once per frame and may return `None` until the world clock is known.
    ///
    /// Without this, an installed sky renders permanently at noon: the sun sits
    /// fixed overhead and the stars/moon never appear.
    pub fn set_time_of_day_source(&mut self, f: impl Fn() -> Option<i64> + Send + Sync + 'static) {
        self.time_of_day = TimeOfDaySource(Some(Box::new(f)));
    }

    /// Install the source for the local player's own third-person body (see
    /// [`ThirdPersonBodyState`]). Unset — the default — `render`/
    /// `render_with_crack` behave exactly as they did before this existed:
    /// the first-person arm draws unconditionally and no extra entity is
    /// added.
    ///
    /// `f` is polled once per frame, exactly like
    /// [`set_entity_light_source`](Self::set_entity_light_source). There is
    /// deliberately no separate "camera mode" setter: return `None` while the
    /// camera is first-person and `Some` only in a third-person mode, once
    /// one exists — that closure *is* the camera-mode toggle referred to
    /// throughout this module's docs. When `f` returns `Some` for a frame:
    ///
    /// * the state is resolved into an [`EntityDraw`]
    ///   ([`ThirdPersonBodyState::into_draw`]) and folded into that frame's
    ///   entity list, so it is posed by the exact same animated
    ///   `Skeleton::pose` chain every tracked mob uses — never
    ///   [`first_person_arm_pose`]'s rest-pose-plus-fixed-rotation chain —
    ///   and any equipment on it renders through the ordinary held-item path,
    ///   for free.
    /// * the first-person arm pass is skipped for that frame. The two must
    ///   never draw together: the arm's pose has no world position at all
    ///   (it is built directly in camera space), so if it drew while a
    ///   third-person camera was active it would stay glued wherever the
    ///   camera looks, pasted over the body it is supposed to be attached to.
    ///
    /// # What this alone does not solve
    ///
    /// This is the render-side half only — `f`'s closure needs a feet
    /// position, body yaw and an [`AnimInput`] for the local player to hand
    /// back, and this shell has none of that today: no third-person camera
    /// mode, no camera offset, and no local-player pose separate from the
    /// camera's own eye. Wiring `f` is `app.rs`/`sim.rs` work — see this
    /// method's crate docs for the exact spec — and is deliberately not done
    /// here, so a third-person body stays at zero pixels until it lands, per
    /// this repo's "nothing is done until something on screen changes" rule.
    pub fn set_third_person_body_source(
        &mut self,
        f: impl Fn() -> Option<ThirdPersonBodyState> + Send + Sync + 'static,
    ) {
        self.third_person_body = ThirdPersonBodySource(Some(Box::new(f)));
    }

    /// Install the source for the first-person arm's swing progress (see
    /// [`HandSwingSource`]).
    ///
    /// Until installed, the arm is drawn permanently at rest, which is what it did
    /// before the swing existed. `f` must return
    /// `Sim::hand_swing_progress`-shaped data: a **tick**-advanced swing clock
    /// read with this frame's partial tick.
    ///
    /// **Re-install it every frame**, alongside
    /// [`set_third_person_body_source`](Self::set_third_person_body_source), which
    /// has the identical requirement: the value is a partial-tick interpolation, so
    /// a one-shot install at connect time freezes the arm at whatever the swing
    /// looked like the instant we joined. Sample first and move the number into the
    /// closure, rather than borrowing the `Sim` — the source outlives the call.
    ///
    /// ```no_run
    /// # fn wire(render: &mut lodestone::gpu::RenderState, sim: &lodestone::sim::Sim) {
    /// let hand_swing = sim.hand_swing_progress();
    /// render.set_hand_swing_source(move || hand_swing);
    /// # }
    /// ```
    pub fn set_hand_swing_source(&mut self, f: impl Fn() -> f32 + Send + Sync + 'static) {
        self.hand_swing = HandSwingSource(Some(Box::new(f)));
    }

    /// Install the source for local item use (see [`ItemUseSource`]), which selects
    /// live item variants such as bow-pulling models and makes consumables dip and
    /// jitter toward the mouth.
    ///
    /// Until installed, a held food is posed exactly like any other item and eating
    /// has no first-person animation at all — the pass still runs and the item still
    /// draws, so a missing install looks like working code with a missing feature
    /// rather than like a failure. That is the island shape, so this is the second
    /// half of the work and not an optional extra.
    ///
    /// `f` returns [`ItemUseState`]. Its `using`/`ticks` pair is the live item-use
    /// state; its `eat` field is vanilla's `getUseItemRemainingTicks() -
    /// frameInterp + 1.0F` paired with the item's own consumable-consume-ticks component.
    ///
    /// **Re-install it every frame**, for the same reason
    /// [`set_hand_swing_source`](Self::set_hand_swing_source) says to: the value
    /// carries this frame's partial tick, so a one-shot install freezes the bob.
    ///
    /// ```no_run
    /// # fn wire(render: &mut lodestone::gpu::RenderState, sim: &lodestone::sim::Sim) {
    /// let item_use = sim.item_use_render_state();
    /// render.set_item_use_source(move || item_use);
    /// # }
    /// ```
    pub fn set_item_use_source(
        &mut self,
        f: impl Fn() -> ItemUseState + Send + Sync + 'static,
    ) {
        self.item_use = ItemUseSource(Some(Box::new(f)));
    }

    /// Install the source for the local player's **main-hand item** (see
    /// [`MainHandSource`]), so first person draws the held item instead of a bare
    /// arm.
    ///
    /// Until installed, the bare arm is drawn unconditionally for an empty
    /// hand. `f` returns the item id of the *selected hotbar slot* together
    /// with whether that stack is enchanted (the foil flag that drives the glint
    /// second pass), or `None` for an empty hand.
    ///
    /// **Re-install it every frame**, for the same reason
    /// [`set_hand_swing_source`](Self::set_hand_swing_source) says to: the value
    /// changes when the player scrolls the hotbar, and a one-shot install at
    /// connect time freezes whatever was in slot 0 at join into the hand forever.
    /// Sample first and move the value into the closure rather than borrowing the
    /// `Sim`, which the source outlives.
    ///
    /// ```no_run
    /// # fn wire(render: &mut lodestone::gpu::RenderState, sim: &lodestone::sim::Sim) {
    /// // `Sim::selected_slot()` indexes the hotbar records `app.rs` already
    /// // builds for the HUD; take that record's item id, `enchanted` flag and
    /// // dye/potion colour.
    /// let held: Option<lodestone::gpu::MainHandItem> = None; // = hotbar[selected]
    /// render.set_main_hand_source(move || held.clone());
    /// # }
    /// ```
    ///
    /// # This also steps the equip/swap animation
    ///
    /// A setter with a side effect, deliberately, and worth reading before moving
    /// it. Vanilla's swap state (`ItemInHandRenderer.mainHandItem` /
    /// `mainHandHeight`) needs to see the selected item *change*, once per unit of
    /// time. This call is exactly that observation: the shell re-installs the source
    /// every in-world frame with this frame's selection, it is the only `&mut self`
    /// hop on that path, and [`RenderState::render`] takes `&self` so the state
    /// cannot be advanced there.
    ///
    /// The alternative — a second per-frame setter carrying an already-computed
    /// height — would have needed a new `app.rs` install to do anything at all, and
    /// a source nobody installs draws nothing: the island `CLAUDE.md` §1 names.
    /// Advancing here means the animation is live for every caller that already
    /// draws a held item, including the existing GPU gates, with no new wiring.
    ///
    /// The source is stored first and then read back through
    /// `MainHandSource::value`, so the equip state observes exactly the value
    /// `prepare_first_person_hand` would have seen — one spelling of "the selected
    /// item", not two. The closure is invoked
    /// once per install and must stay cheap and side-effect-free (a clone of an
    /// `Option<(ResourceLocation, bool)>`, as the example above), which it
    /// already is for every caller.
    pub fn set_main_hand_source(
        &mut self,
        f: impl Fn() -> Option<super::MainHandItem> + Send + Sync + 'static,
    ) {
        self.main_hand = MainHandSource(Some(Box::new(f)));
        let selected = self.main_hand.value();
        self.equip.advance(selected.as_ref());
    }

    /// Install the source for this frame's block entities (including chests).
    ///
    /// **Without this every chest in the world is an invisible hole.** A 26.2
    /// chest has no block model at all (`block/chest.json` declares only a
    /// particle texture, zero elements), so the terrain mesher draws nothing
    /// there and only this pass can. That makes it the one source here whose
    /// absence is a missing *block*, not a missing effect.
    ///
    /// The closure receives the camera position, because vanilla's own gate is
    /// per-block-entity distance from the camera and applying it where the world
    /// is walked is much cheaper than filtering afterwards.
    ///
    /// **Re-install every frame**, like [`Self::set_main_hand_source`] and unlike
    /// [`Self::set_entity_light_source`]: the lid angle is partial-tick
    /// interpolated, so a closure installed once at connect draws every chest
    /// frozen at the fraction of a tick it happened to be installed on.
    pub fn set_block_entity_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::ChestSpawn> + Send + Sync + 'static,
    ) {
        self.block_entity_source = BlockEntitySource(Some(Box::new(f)));
    }

    /// Install the source for this frame's skulls and heads — the skull
    /// equivalent of [`set_block_entity_source`](Self::set_block_entity_source).
    ///
    /// A second field rather than a second return value on the chest closure:
    /// the two gathers are independent functions with no shared per-frame state,
    /// and a skull needs no partial tick because none of the ported types
    /// animate. See [`SkullSource`].
    pub fn set_skull_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::SkullSpawn> + Send + Sync + 'static,
    ) {
        self.skull_source = SkullSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's copper golem statues — the
    /// statue equivalent of [`set_skull_source`](Self::set_skull_source): a
    /// real cuboid rig through `prepare_block_entities`, no clock, no
    /// per-position tracker (pose/oxidation/facing are all block-state/
    /// block-name driven).
    pub fn set_copper_golem_statue_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::CopperGolemStatueSpawn> + Send + Sync + 'static,
    ) {
        self.copper_golem_statue_source = CopperGolemStatueSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's bells — the bell equivalent of
    /// [`set_skull_source`](Self::set_skull_source).
    ///
    /// Must be re-installed every frame: the closure captures the partial tick,
    /// which the shake angle interpolates against, so a stale one stutters the
    /// swing at the frame rate. `app::redraw` does this from `Sim::bell_source`.
    pub fn set_bell_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::BellSpawn> + Send + Sync + 'static,
    ) {
        self.bell_source = BellSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's spawner/trial-spawner display
    /// mobs — the spawner equivalent of [`set_bell_source`](Self::set_bell_source).
    ///
    /// Must be re-installed every frame for the same reason: the closure
    /// captures the partial tick the spin angle interpolates against.
    /// `app::redraw` does this from `Sim::spawner_source`. Unlike every
    /// other source in this family, leaving it unset draws nothing *extra*
    /// rather than leaving a hole — both the mob spawner and the trial
    /// spawner have real block-model geometry for the cage, drawn by the
    /// ordinary terrain mesher regardless of this source.
    pub fn set_spawner_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::SpawnerMobSpawn> + Send + Sync + 'static,
    ) {
        self.spawner_source = SpawnerSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's shulker boxes — the shulker
    /// equivalent of [`set_bell_source`](Self::set_bell_source), and the
    /// thinnest of the family: no clock, no animation map.
    ///
    /// Leaving it unset is a **hole in the world**, not a missing decoration: a
    /// 26.2 shulker box declares no block model, so the terrain mesher draws
    /// nothing at all where one stands. Same failure mode as chest.
    pub fn set_shulker_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::ShulkerSpawn> + Send + Sync + 'static,
    ) {
        self.shulker_source = ShulkerSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's decorated pots — the decorated-pot
    /// equivalent of [`set_shulker_source`](Self::set_shulker_source).
    ///
    /// Leaving it unset is a **hole in the world**, not a missing decoration:
    /// a 26.2 decorated pot's block model is `assets/minecraft/models/block/
    /// decorated_pot.json`, and every visible triangle — base *and* sides —
    /// comes from this pass. Same failure mode as chest and shulker.
    pub fn set_decorated_pot_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::DecoratedPotSpawn> + Send + Sync + 'static,
    ) {
        self.decorated_pot_source = DecoratedPotSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's conduits — the conduit equivalent
    /// of [`set_shulker_source`](Self::set_shulker_source).
    ///
    /// Leaving it unset is a **hole in the world**, not a missing decoration:
    /// a conduit's block model declares real geometry and every visible
    /// triangle comes from this pass, same failure mode as chest, shulker and
    /// decorated pot. Unlike those, the closure this installs must itself carry
    /// a per-position tick tracker — see [`ConduitSource`]'s doc — because
    /// `isActive`/`isHunting` and the rotation counters are computed
    /// client-side from the block store, not read off the wire.
    pub fn set_conduit_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::ConduitSpawn> + Send + Sync + 'static,
    ) {
        self.conduit_source = ConduitSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's banners.
    ///
    /// Must be re-installed every frame: the closure captures the game tick and the
    /// partial tick, so a stale one freezes every banner's cloth mid-sway — the
    /// same hazard [`set_bell_source`](Self::set_bell_source) documents.
    pub fn set_banner_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::BannerSpawn> + Send + Sync + 'static,
    ) {
        self.banner_source = BannerSource(Some(Box::new(f)));
    }

    /// Upload every remote player's skin that has finished fetching since the
    /// last frame, as a texture bind group keyed by its URL.
    ///
    /// Not a `set_*_source`: this is a **one-way upload**, not a per-frame
    /// closure. It has to be a `&mut self` call from outside the frame because
    /// creating a bind group needs the device and the render pass borrows
    /// everything immutably. Cheap on all but the handful of frames after a fetch
    /// lands, since [`crate::remote_skins::drain_ready`] is empty otherwise.
    ///
    /// **Not optional, and its absence is invisible in a screenshot**: without
    /// this call every remote player draws the pack's default sheet forever, which
    /// is exactly what an offline-mode server legitimately looks like. See
    /// `crate::remote_skins`.
    pub fn install_pending_player_skins(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.entities.install_pending_player_skins(device, queue);
    }

    /// Install the source for this frame's lectern books — the lectern
    /// equivalent of [`set_shulker_source`](Self::set_shulker_source), and like
    /// it, free of any clock: `LECTERN_BOOK_OPENNESS` is a compile-time constant
    /// in the jar, so nothing about a lectern book varies with time.
    ///
    /// Leaving it unset is the mildest degradation of the family — an empty
    /// lectern rather than a hole, since the shelf and base are real block
    /// models. It is still an island if nobody calls it, which is why
    /// `app::redraw` installs it beside the other four.
    pub fn set_lectern_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::LecternSpawn> + Send + Sync + 'static,
    ) {
        self.lectern_source = LecternSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's campfire cooking items.
    ///
    /// Clock-free like [`set_lectern_source`](Self::set_lectern_source), and for a
    /// stronger reason: `CampfireRenderer` has no animation whatsoever — the flame
    /// flicker belongs to the block model's animated texture, and the NBT's
    /// `CookingTimes` drive nothing on the client.
    ///
    /// The one asymmetry worth knowing: this feeds
    /// [`prepare_item_geometry`](Self::prepare_item_geometry) and the model
    /// pipeline rather than `prepare_block_entities` and the entity pipeline, so
    /// a campfire item is textured from the *block atlas* exactly like the same
    /// item lying on the ground.
    pub fn set_campfire_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::CampfireItemSpawn> + Send + Sync + 'static,
    ) {
        self.campfire_source = CampfireSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's vault display-item clusters.
    ///
    /// **Must be re-installed every frame**, unlike
    /// [`set_campfire_source`](Self::set_campfire_source): the spin advances
    /// every tick (`lodestone_render::entity::vault_spin_degrees`), so a stale
    /// closure freezes it at the tick it was captured — the same requirement
    /// [`set_beacon_source`](Self::set_beacon_source) documents for the beam's
    /// own rotating core.
    ///
    /// Also feeds [`prepare_item_geometry`](Self::prepare_item_geometry) and
    /// the model pipeline, not `prepare_block_entities` — see
    /// [`VaultSource`]'s doc for why: the vault's cage is real block-model
    /// geometry, and only the floating reward comes from here.
    pub fn set_vault_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::VaultSpawn> + Send + Sync + 'static,
    ) {
        self.vault_source = VaultSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's brushable-block revealed items.
    ///
    /// Clock-free like [`set_campfire_source`](Self::set_campfire_source) — a
    /// revealed item does not animate — and feeds
    /// [`prepare_item_geometry`](Self::prepare_item_geometry) rather than
    /// `prepare_block_entities`, for the same odd-one-out reason
    /// [`BrushableSource`]'s doc gives: the suspicious sand/gravel a player
    /// sees is entirely a real block model.
    pub fn set_brushable_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::BrushableItemSpawn> + Send + Sync + 'static,
    ) {
        self.brushable_source = BrushableSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's shelved items.
    ///
    /// Clock-free like [`set_brushable_source`](Self::set_brushable_source) —
    /// a shelved item does not animate — and feeds
    /// [`prepare_item_geometry`](Self::prepare_item_geometry) rather than
    /// `prepare_block_entities`, for the same odd-one-out reason
    /// [`ShelfSource`]'s doc gives.
    pub fn set_shelf_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::ShelfItemSpawn> + Send + Sync + 'static,
    ) {
        self.shelf_source = ShelfSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's moving pistons — vanilla's
    /// `PistonHeadRenderer`.
    ///
    /// **Must be re-installed every frame**, and the failure mode is worse than
    /// any other source's here: a whole push lasts *two* ticks
    /// (`PistonMovingBlockEntity.TICKS_TO_EXTEND`), and a stale closure pins
    /// `progress` at 0, which draws the head one full cell back **inside** the
    /// piston base rather than merely freezing it.
    ///
    /// Unlike [`set_campfire_source`](Self::set_campfire_source) — the other source
    /// that bypasses `prepare_block_entities` — this one does not reach the item
    /// path either. It feeds
    /// [`prepare_moving_blocks`](Self::prepare_moving_blocks), sharing one vertex
    /// buffer and one draw call with falling blocks.
    pub fn set_moving_piston_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::MovingPistonSpawn> + Send + Sync + 'static,
    ) {
        self.moving_piston_source = MovingPistonSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's enchanting-table books.
    ///
    /// **Must be re-installed every frame**, more strictly than any other source
    /// in this family: the closure captures both the animation fold and the partial
    /// tick, and *nothing* about an enchanting table's book is on the wire — so a
    /// stale closure freezes every book in the world and there is no missing packet
    /// to blame it on.
    pub fn set_enchanting_table_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::EnchantingTableSpawn> + Send + Sync + 'static,
    ) {
        self.enchanting_table_source = EnchantingTableSource(Some(Box::new(f)));
    }

    /// Install this frame's `bobHurt` eye-space transform — the damage tilt and
    /// the death roll (`crate::camera_rig::BobFrame::hurt_transform`).
    ///
    /// A value, not a closure, unlike the sources above: it is one small matrix
    /// the caller already has, and there is nothing to gather. Still **per
    /// frame**, and for the sharpest reason in this module: the tilt decays over
    /// ten ticks with a partial-tick term, so a one-shot install freezes the
    /// camera at whatever angle it was first handed — including, if that happened
    /// to be mid-flash, permanently.
    ///
    /// Pass `Mat4::IDENTITY` to disable, which is exactly what an unhurt frame
    /// produces anyway. Callers that never install anything get the identity and
    /// therefore bit-identical matrices to before this existed.
    pub fn set_eye_bob_transform(&mut self, eye_bob: glam::Mat4) {
        self.eye_bob = eye_bob;
    }

    /// Push vanilla's Damage Tilt accessibility option down for the
    /// **first-person hand** pass, which applies `bobHurt` a second time.
    ///
    /// The world's copy needs no equivalent: it arrives already multiplied out in
    /// [`set_eye_bob_transform`](Self::set_eye_bob_transform)'s matrix. Clamped, so
    /// a caller cannot ask for a tilt vanilla could not produce.
    pub fn set_damage_tilt_strength(&mut self, strength: f32) {
        self.damage_tilt_strength = strength.clamp(0.0, 1.0);
    }

    /// This frame's world view-projection: `P · bobHurt · V`.
    ///
    /// **Every world-space uniform this state writes must go through here**, not
    /// through `Camera::view_projection` — the entity pass, the block-entity pass
    /// and the world glint each write their own group 0, and a pass that skipped
    /// the tilt would slide against the terrain around it while the camera leaned.
    /// That is a far more visible defect than no tilt at all, and it is the reason
    /// this is a method rather than four call sites composing the product.
    ///
    /// `render_inner` composes the nausea/portal spin on top of this, in vanilla's
    /// order (`P · bob · warp · V`), which is why that one site calls
    /// `Camera::view_projection_eye_space` directly instead.
    #[must_use]
    pub(super) fn world_view_projection(&self, camera: &Camera) -> glam::Mat4 {
        camera.view_projection_eye_space(self.eye_bob)
    }

    /// This frame's installed `bobHurt` transform — for `render_inner`, which
    /// needs to compose the spinning warp onto it.
    #[must_use]
    pub(super) fn eye_bob(&self) -> glam::Mat4 {
        self.eye_bob
    }

    /// Install the source for this frame's filled-map pictures.
    ///
    /// Re-installed every frame like the block-entity sources, and for a sharper
    /// reason than theirs: the closure captures a **snapshot** of `SessionMaps`, so
    /// one installed at login would show a map frozen at whatever the server had
    /// sent by then and would never fill in as the player explored.
    pub fn set_map_source(
        &mut self,
        f: impl Fn(Option<i32>, Option<i32>) -> Option<super::MapPicture> + Send + Sync + 'static,
    ) {
        self.map_source = MapSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's sign text — same shape as
    /// [`set_skull_source`](Self::set_skull_source): an independent gather,
    /// no shared per-frame state, and no partial-tick interpolation because
    /// sign text does not animate.
    pub fn set_sign_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::SignSpawn> + Send + Sync + 'static,
    ) {
        self.sign_source = SignSource(Some(Box::new(f)));
    }

    /// Install this frame's extracted `Display`-family entities
    /// (`text_display`/`item_display`/`block_display`) — `app::redraw` calls
    /// this every frame with `Sim::display_draws()`
    /// (`crate::display_entities::extracted_display_draws(world)` underneath),
    /// the same way `entities: &[EntityDraw]` already reaches
    /// [`RenderState::render`] from `extracted_entity_draws`. Unlike that
    /// one, this is a setter rather than a `render` parameter: threading a
    /// sixth top-level per-frame input through `render`'s own already-long
    /// signature (and every helper it calls) would touch far more of this
    /// file for the same information a two-line setter already carries.
    ///
    /// **This had zero production callers for a while after it was
    /// written** — the extract system, the ingest fold and the
    /// `text_display` GPU pass below were all wired and individually tested,
    /// and nothing above this method ever called it, so a real `text_display`
    /// resolved all the way to a `DisplayDraw` and then never reached the
    /// screen. `app::redraw`'s call site says so at the point it was added;
    /// this note stays as the reason a doc-comment claim of "the caller is
    /// expected to…" is not evidence a caller exists.
    /// # All three subtypes are consumed
    ///
    /// This setter used to warn once per entity id for any `type_path` other
    /// than `text_display`, because nothing downstream read those: an
    /// `item_display` or `block_display` resolved all the way to a draw-ready
    /// snapshot and then dropped off the edge of the pipeline. That gap is
    /// closed — `gpu/moving_blocks.rs`'s `merge_block_displays` and
    /// `gpu/world_items.rs`'s `merge_item_displays` both read
    /// [`Self::display_draws`] now — so the warning and the set of ids it
    /// deduplicated against are gone rather than left to say something untrue.
    pub fn set_display_draws(&mut self, draws: Vec<crate::display_entities::DisplayDraw>) {
        self.display_draws = draws;
    }

    /// Install the source for this frame's beacon beams — same shape as
    /// [`set_sign_source`](Self::set_sign_source). Must be re-installed every
    /// frame: the closure captures the game tick and the partial tick the
    /// beam's scroll/spin animate against, so a stale install freezes it.
    /// `app::redraw` does this from `Sim::beacon_source`.
    pub fn set_beacon_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::BeaconSpawn> + Send + Sync + 'static,
    ) {
        self.beacon_source = BeaconSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's end portals — same shape as
    /// [`set_sign_source`](Self::set_sign_source): no per-frame animation
    /// state to go stale, so unlike [`set_beacon_source`](Self::set_beacon_source)
    /// there is no *requirement* to reinstall every frame, though
    /// `app::redraw` does anyway alongside every other block-entity source
    /// for uniformity. `app::redraw` calls this from `Sim::end_portal_source`.
    pub fn set_end_portal_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::EndPortalSpawn> + Send + Sync + 'static,
    ) {
        self.end_portal_source = EndPortalSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's end gateways — same shape as
    /// [`set_end_portal_source`](Self::set_end_portal_source).
    /// `app::redraw` calls this from `Sim::end_gateway_source`.
    pub fn set_end_gateway_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::EndGatewaySpawn> + Send + Sync + 'static,
    ) {
        self.end_gateway_source = EndGatewaySource(Some(Box::new(f)));
    }

    /// Install the source for this frame's end gateway teleport beams.
    ///
    /// **Must be re-installed every frame**, like
    /// [`set_bell_source`](Self::set_bell_source): the closure captures a
    /// snapshot of the `teleportCooldown` tracker plus the game/partial
    /// tick, so a stale install both freezes an in-progress countdown and
    /// leaves the spawn arm's smoothing pinned at the tick it was captured.
    pub fn set_end_gateway_beam_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::EndGatewayBeamSpawn> + Send + Sync + 'static,
    ) {
        self.end_gateway_beam_source = EndGatewayBeamSource(Some(Box::new(f)));
    }

    /// Sets the end-portal/end-gateway star-field shader's `GameTime` term —
    /// an ever-increasing tick clock, unlike [`set_beacon_source`](Self::set_beacon_source)'s
    /// own `floorMod(40)`-wrapped `animation_time`. A plain scalar rather
    /// than a closure-backed source, since the swirl needs it every frame
    /// regardless of which (if any) portals are in view.
    pub fn set_end_portal_game_time(&mut self, game_time: f32) {
        self.end_portal_game_time = game_time;
    }

    /// Install the source for the targeted block's outline shape.
    ///
    /// Without this the selection box is a unit cube, which is wrong for roughly
    /// nine block states in ten — only 3,328 of 32,366 have a full-cube outline.
    pub fn set_outline_shape_source(
        &mut self,
        f: impl Fn([i32; 3]) -> Vec<lodestone_physics::Aabb> + Send + Sync + 'static,
    ) {
        self.outline_shape = OutlineShapeSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's world-space debug lines (see
    /// [`DebugLinesSource`]). Until installed, [`render`](Self::render) draws
    /// none — this pass is a real pipeline, not a stub, but it is wired to
    /// nothing until a caller supplies geometry here (typically once, at
    /// connect time, next to [`set_outline_shape_source`](Self::set_outline_shape_source)).
    /// The source receives the camera position so view-relative debug geometry
    /// can use vanilla's camera-distance rules.
    pub fn set_debug_lines_source(
        &mut self,
        f: impl Fn(glam::Vec3) -> Vec<DebugLineVertex> + Send + Sync + 'static,
    ) {
        self.debug_lines_source = DebugLinesSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's plugin billboards (see
    /// [`PluginBillboardsSource`]) — [`set_debug_lines_source`](Self::set_debug_lines_source)'s
    /// sibling for textured/billboard channel. Until installed,
    /// [`render`](Self::render) draws none: this pass is a real pipeline, not
    /// a stub, but wired to nothing until a caller polls
    /// `lodestone_ecs::PluginBillboards` and hands the result here.
    pub fn set_plugin_billboards_source(
        &mut self,
        f: impl Fn() -> Vec<PluginBillboardInstance> + Send + Sync + 'static,
    ) {
        self.plugin_billboards_source = PluginBillboardsSource(Some(Box::new(f)));
    }

    /// This renderer's block-atlas sprite table (`ResourceLocation` string →
    /// UV rect) — what [`crate::gpu::plugin_billboard_vertices`] resolves
    /// [`lodestone_ecs::PluginTexture::Named`] against. A cheap `Arc` clone,
    /// meant to be captured once by the closure
    /// `install_plugin_billboards_source` (`app/session.rs`) hands to
    /// [`set_plugin_billboards_source`](Self::set_plugin_billboards_source),
    /// mirroring how that same install captures the world column for the F3
    /// chunk-border debug line.
    #[must_use]
    pub fn plugin_atlas_sprites(&self) -> std::sync::Arc<HashMap<String, [f32; 4]>> {
        std::sync::Arc::clone(&self.plugin_atlas_sprites)
    }

    /// Upload this frame's particle instances. Must run before
    /// [`render`](Self::render), which only records the draw — a render pass
    /// cannot create buffers.
    pub fn prepare_particles(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[ParticleInstance],
        camera: &Camera,
    ) {
        self.particles.prepare(device, queue, instances, camera);
        // The one asymmetry the type system cannot close: `Particles` gets its
        // sheet UV table from `Sim` and this pass gets the sheet *texture* from
        // `app.rs`, so a build that wires one and not the other resolves sheet
        // particles that then sample a transparent stand-in and vanish. Said
        // once, loudly, instead of leaving it to look like an idle frame.
        if self.particle_sheet_atlas.is_none()
            && self.particles.sheet_count() > 0
            && !self.warned_missing_particle_sheet
        {
            self.warned_missing_particle_sheet = true;
            tracing::warn!(
                target: "particles",
                sheet_instances = self.particles.sheet_count(),
                "sheet particles resolved but no particle sheet is bound; they will draw \
                 nothing. Call RenderState::install_particle_sheet_atlas with the same \
                 ParticleAtlas Particles::with_particle_atlas was given."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RenderState;

    /// The **decision** half of the world-text gamma fix, with no GPU in it.
    ///
    /// `RenderState::set_world_text_view` picks the colour format the three
    /// flat-colour world-text passes are built for, and the view it hands them,
    /// from one expression — so this asserts that expression against a
    /// native-shaped format and a web-shaped one, the two cases
    /// `lodestone_render::target`'s own `choose_view_format` gates enumerate.
    ///
    /// A decision-level assertion rather than a composited byte on purpose:
    /// this codebase has measured `ALPHA_BLENDING` on Metal as a non-trivial
    /// function of the fragment alpha that resists a closed form, so "which
    /// format did the code pick" is a claim that can be checked exactly while
    /// "which byte came out" cannot.
    #[test]
    fn world_text_targets_the_raw_counterpart_of_a_native_swapchain_format() {
        // What native `wgpu-core`'s `Surface::get_default_config` actually
        // picks, and what `SurfaceTarget::format` therefore reports.
        assert_eq!(
            RenderState::gamma_text_format(wgpu::TextureFormat::Bgra8UnormSrgb),
            wgpu::TextureFormat::Bgra8Unorm,
            "on a native sRGB swapchain the world's text must composite through the raw \
             (non-sRGB) view of the same texture — that is the whole fix"
        );
        assert_eq!(
            RenderState::gamma_text_format(wgpu::TextureFormat::Rgba8UnormSrgb),
            wgpu::TextureFormat::Rgba8Unorm,
        );
    }

    /// The web-shaped half: a browser canvas structurally cannot be configured
    /// with an sRGB format (`wgpu`'s `WebSurface::get_capabilities` never lists
    /// one), so `config.format` there is already raw and the decision must be
    /// a no-op rather than an attempt to strip a suffix that is not present.
    /// Getting this wrong would name a format the swapchain never declared in
    /// `view_formats`, which is a validation abort on the one platform no gate
    /// here runs on.
    #[test]
    fn world_text_leaves_an_already_raw_format_alone() {
        for format in [
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba16Float,
        ] {
            assert_eq!(
                RenderState::gamma_text_format(format),
                format,
                "{format:?} is already non-sRGB: the raw view and the target's own view are \
                 the same view, so there is nothing to reinterpret"
            );
        }
    }
}
