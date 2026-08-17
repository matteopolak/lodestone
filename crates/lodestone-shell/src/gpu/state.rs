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

use super::first_person;
use super::terrain::{
    MODEL_ORIGIN_ARENA_SLOTS, ModelRenderer, PACKED_ORIGIN_ARENA_SLOTS, SectionOriginArena,
    anim_slots_at,
};
use super::{
    AmbientLightSource, BannerSource, BeaconBeamRenderer, BeaconSource, BellSource,
    BlockEntityRenderer, BlockEntitySource, CampfireSource, ConduitSource,
    DEFAULT_RENDER_DISTANCE_CHUNKS, DecoratedPotSource, EnchantingTableSource, ShulkerSource,
    DebugLineRenderer, DebugLineVertex, DebugLinesSource, EntityLightSource, EntityRenderer,
    HandSwingSource, ItemUseSource, LecternSource, MainHandSource, MapSource, MovingPistonSource,
    NameTagRenderer, OutlineRenderer, OutlineShapeSource,
    PluginBillboardInstance, PluginBillboardRenderer, PluginBillboardsSource,
    RenderState, SKY_COLOR, SignSource, SignTextRenderer, SkullSource, SkyDarkenSource,
    ThirdPersonBodySource, ThirdPersonBodyState, TimeOfDaySource, transparent_placeholder_atlas,
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
                let atlas = GpuAtlas::from_atlas(device, queue, va.atlas());
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
        // The packed path's shared camera + per-section origin arena (issue
        // #76). One bind group over both, built once here; every packed section
        // draw reuses it and varies only the dynamic offset.
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
        let beacon_beam = BeaconBeamRenderer::new(device, queue, color_format);

        // The live vanilla atlas carries baked model geometry; build the model
        // render pass over its *complete* atlas (whose UVs the baked quads index,
        // distinct from the cube atlas bound above). The demo path has no models,
        // so this stays `None` and terrain draws through the packed pipeline.
        let model = vanilla.and_then(BlockAtlas::models).map(|models| {
            let pipeline = ModelPipeline::new(device, color_format);
            let water_pipeline = ModelPipeline::for_fluid(device, color_format);
            // Translucent **block** geometry (stained glass, ice, the nether
            // portal swirl): the same MODEL_WGSL shader and palette as
            // `pipeline`, alpha-blended instead of cutout-discarded. Distinct
            // from `water_pipeline`, whose `FLUID_WGSL` shader has no palette
            // and always applies the water tint — wrong for a palette-tinted
            // translucent block.
            let translucent_pipeline =
                ModelPipeline::for_layer(device, color_format, lodestone_render::RenderLayer::Translucent);
            let atlas = GpuAtlas::from_atlas(device, queue, models.atlas());
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
            // fog) and the per-section origin arena (issue #75 — see the
            // module doc). One bind group over both, built once; every
            // section draw and the dropped-item pass reuse it, varying only
            // the dynamic offset.
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
        // it draws *nothing*, which is wrong but honest, instead of the
        // arbitrary block texels issue #45 reported.
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
            sections: HashMap::new(),
            packed_shared_cam_buffer,
            packed_cam_bind_group,
            packed_origin_arena,
            model,
            outline,
            debug_lines,
            debug_lines_source: DebugLinesSource::default(),
            plugin_billboards,
            plugin_billboards_source: PluginBillboardsSource::default(),
            plugin_atlas_sprites: std::sync::Arc::new(plugin_atlas_sprites),
            entities,
            flame_frame_counter: std::cell::Cell::new(0),
            section_fade_tick: std::cell::Cell::new(0),
            particles,
            particle_atlas_bind_group,
            particle_sheet_atlas: None,
            warned_missing_particle_sheet: false,
            // Full-bright until the shell installs a world sampler; see
            // `set_entity_light_source`.
            entity_light: EntityLightSource::default(),
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
            block_entities: BlockEntityRenderer::new(device, queue, color_format),
            // No chests until the shell installs a world source; see
            // `set_block_entity_source`.
            block_entity_source: BlockEntitySource::default(),
            skull_source: SkullSource::default(),
            // No bells until a caller installs a world source; see
            // `set_bell_source`, installed per frame by `app::redraw`.
            bell_source: BellSource::default(),
            // Likewise `set_shulker_source`.
            shulker_source: ShulkerSource::default(),
            // Likewise `set_decorated_pot_source`.
            decorated_pot_source: DecoratedPotSource::default(),
            // Likewise `set_conduit_source`.
            conduit_source: ConduitSource::default(),
            // Likewise `set_map_source` (issue #184).
            map_source: MapSource::default(),
            // Likewise `set_banner_source`.
            banner_source: BannerSource::default(),
            // Likewise `set_lectern_source`.
            lectern_source: LecternSource::default(),
            // Likewise `set_campfire_source`.
            campfire_source: CampfireSource::default(),
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
            sign_text,
            // No signs until the shell installs a world source; see
            // `set_sign_source`.
            sign_source: SignSource::default(),
            beacon_beam,
            // No beacon beams until the shell installs a world source; see
            // `set_beacon_source`.
            beacon_source: BeaconSource::default(),
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
            // render distance, on vanilla's own span (issue #388) rather than a
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
            vis_graph: lodestone_render::VisibilityGraph::new(),
            occlusion: std::cell::RefCell::default(),
            last_camera_block_pos: std::cell::Cell::new(None),
            // Likewise on by default, for the same reason — and it is harmless
            // until something populates the graph: an empty graph produces no
            // reachable set, which is the pre-U3 cull exactly. See
            // `TerrainOcclusion` for the two weaker settings and when to reach
            // for them.
            occlusion_mode: super::TerrainOcclusion::On,
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

    /// Replace the distance-fog settings (colour + range) **and the sky disc's
    /// centre colour**, which travel together in [`FogSettings`]. The shell
    /// drives this from its configured render distance and the eye-in-fluid
    /// state: a sky-coloured fog sized to the render distance normally, a short
    /// biome-coloured water fog when submerged. Pass [`FogSettings::disabled`]
    /// to turn fog off.
    ///
    /// `FogSettings::sky_color` is what the sky pass paints the disc centre with
    /// (issue #96's per-biome tint). It is in the same struct rather than behind
    /// its own setter precisely so a caller cannot update one and forget the
    /// other — see [`FogSettings`]' doc and
    /// [`set_clear_color`](Self::set_clear_color) below.
    /// `render_distance_chunks` rides along for the same reason `sky_color` is in
    /// the struct: the sky disc's gradient end is `min(render_distance, the
    /// attribute)` (`AtmosphericFogEnvironment.java`), so it is a *second*
    /// consumer of the same number the fog band already needs. #399 shipped the
    /// gradient clamp with `SkyFrame` defaulting to the old constant 512 and this
    /// call site still passing it — the mechanism landed and reached zero pixels.
    /// Taking it as a parameter rather than adding a `set_render_distance` next
    /// door makes that unrepresentable: you cannot set fog without saying what
    /// distance it is for.
    pub fn set_fog(&mut self, fog: FogSettings, render_distance_chunks: u32) {
        self.fog = fog;
        self.render_distance_chunks = render_distance_chunks;
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
    /// ambient floor: the reported "the entire Nether seems very dark
    /// compared to vanilla", since the Nether's real floor
    /// (`#302821`) is markedly brighter than the overworld's (`#0a0a0a`).
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
    /// (issue #452) — same caller/IO split as [`install_sky`](Self::install_sky):
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
                )),
            );
        }
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
    /// `DimensionType.skybox()`. Read by the sky pass in `gpu/frame.rs` when it
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

    /// Upload the stitched particle sheet and rebind the particle pass to it
    /// (issue #45).
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
    /// needs `Particles`' own UV table rebuilt in the same step (issue #45's
    /// exact trap), which is session (`Sim`) state this module cannot reach.
    /// Entity textures, the item atlas and the GUI/menu atlases are separate
    /// owners entirely — see `crate::app::lifecycle` for those.
    pub fn reload_block_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, vanilla: &BlockAtlas) {
        let new_atlas = GpuAtlas::from_atlas(device, queue, vanilla.atlas());
        let new_uv_buffer = sprite_uv_buffer(device, vanilla.uv_table());
        let new_atlas_bind_group = self
            .pipeline
            .atlas_bind_group(device, &new_atlas, &new_uv_buffer);
        self.atlas = new_atlas;
        self.uv_buffer = new_uv_buffer;
        self.atlas_bind_group = new_atlas_bind_group;

        match (vanilla.models(), self.model.as_mut()) {
            (Some(models), Some(model)) => {
                let new_model_atlas = GpuAtlas::from_atlas(device, queue, models.atlas());
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

    /// Install the source for an in-progress eat or drink (see [`ItemUseSource`]),
    /// which is what makes the held item dip and jitter toward the mouth.
    ///
    /// Until installed, a held food is posed exactly like any other item and eating
    /// has no first-person animation at all — the pass still runs and the item still
    /// draws, so a missing install looks like working code with a missing feature
    /// rather than like a failure. That is the island shape, so this is the second
    /// half of the work and not an optional extra.
    ///
    /// `f` returns `(currUsageTime, useDuration)`: vanilla's
    /// `getUseItemRemainingTicks() - frameInterp + 1.0F` — build it with
    /// [`lodestone_render::entity::eat_usage_time`] rather than by hand — and the
    /// item's `Consumable.consumeTicks()`. `None` means nothing is being consumed.
    ///
    /// **Re-install it every frame**, for the same reason
    /// [`set_hand_swing_source`](Self::set_hand_swing_source) says to: the value
    /// carries this frame's partial tick, so a one-shot install freezes the bob.
    ///
    /// ```no_run
    /// # fn wire(render: &mut lodestone::gpu::RenderState, sim: &lodestone::sim::Sim) {
    /// let eating = sim.consume_usage_time();
    /// render.set_item_use_source(move || eating);
    /// # }
    /// ```
    pub fn set_item_use_source(
        &mut self,
        f: impl Fn() -> Option<(f32, u32)> + Send + Sync + 'static,
    ) {
        self.item_use = ItemUseSource(Some(Box::new(f)));
    }

    /// Install the source for the local player's **main-hand item** (see
    /// [`MainHandSource`]), so first person draws the held item instead of a bare
    /// arm.
    ///
    /// Until installed, the bare arm is drawn unconditionally — vanilla's
    /// empty-hand branch — which is what this shell did before the item path
    /// existed. `f` returns the item id of the *selected hotbar slot* together
    /// with whether that stack is enchanted (the foil flag that drives the glint
    /// second pass, issue #452), or `None` for an empty hand.
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
    /// # This also steps the equip/swap animation (issue #366)
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

    /// Install the source for this frame's block entities (chests, issue #23).
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

    /// Install the source for this frame's filled-map pictures (issue #184).
    ///
    /// Re-installed every frame like the block-entity sources, and for a sharper
    /// reason than theirs: the closure captures a **snapshot** of `SessionMaps`, so
    /// one installed at login would show a map frozen at whatever the server had
    /// sent by then and would never fill in as the player explored.
    pub fn set_map_source(
        &mut self,
        f: impl Fn(Option<i32>) -> Option<Vec<u8>> + Send + Sync + 'static,
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
    /// nothing until a caller polls `lodestone_ecs::player::DebugLines` and
    /// hands the result here (typically once, at connect time, next to
    /// [`set_outline_shape_source`](Self::set_outline_shape_source)).
    pub fn set_debug_lines_source(
        &mut self,
        f: impl Fn() -> Vec<DebugLineVertex> + Send + Sync + 'static,
    ) {
        self.debug_lines_source = DebugLinesSource(Some(Box::new(f)));
    }

    /// Install the source for this frame's plugin billboards (see
    /// [`PluginBillboardsSource`]) — [`set_debug_lines_source`](Self::set_debug_lines_source)'s
    /// sibling for issue #161's textured/billboard channel. Until installed,
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
                 ParticleAtlas Particles::with_particle_atlas was given (issue #45)."
            );
        }
    }
}
