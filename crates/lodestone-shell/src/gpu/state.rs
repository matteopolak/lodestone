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
    BellSource, BlockEntityRenderer, BlockEntitySource, DEFAULT_RENDER_DISTANCE_CHUNKS,
    DebugLineRenderer, DebugLineVertex, DebugLinesSource, EntityLightSource, EntityRenderer,
    HandSwingSource, MainHandSource, NameTagRenderer, OutlineRenderer, OutlineShapeSource,
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
        let entities = EntityRenderer::new(device, queue, color_format);
        let nametag = NameTagRenderer::new(device, color_format);
        let sign_text = SignTextRenderer::new(device, color_format);

        // The live vanilla atlas carries baked model geometry; build the model
        // render pass over its *complete* atlas (whose UVs the baked quads index,
        // distinct from the cube atlas bound above). The demo path has no models,
        // so this stays `None` and terrain draws through the packed pipeline.
        let model = vanilla.and_then(BlockAtlas::models).map(|models| {
            let pipeline = ModelPipeline::new(device, color_format);
            let water_pipeline = ModelPipeline::for_fluid(device, color_format);
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
                hand_cam_buffer,
                hand_cam_bind_group,
                sections: HashMap::new(),
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
            entities,
            flame_frame_counter: std::cell::Cell::new(0),
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
            // No third-person camera exists yet; see
            // `set_third_person_body_source`.
            third_person_body: ThirdPersonBodySource::default(),
            // A rested arm until the shell installs its tick-driven swing clock;
            // see `set_hand_swing_source`.
            hand_swing: HandSwingSource::default(),
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
            // `set_bell_source`. Nothing in this workspace installs one yet
            // (see that method's doc), so this stays empty in the live
            // client today — a hermetic test can still set it directly.
            bell_source: BellSource::default(),
            sign_text,
            // No signs until the shell installs a world source; see
            // `set_sign_source`.
            sign_source: SignSource::default(),
            // No rain/snow droplets until the shell installs the two environment
            // textures; see `install_weather`.
            weather: None,
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
        }
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
    /// attribute)` (`AtmosphericFogEnvironment.java:73`), so it is a *second*
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
        Self::fog_uniform_for(&self.fog, self.time_of_day.value(), self.sky_darken.value(), [
            eye.x, eye.y, eye.z,
        ])
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
        eye: [f32; 3],
    ) -> FogUniform {
        let mut settings = *fog;
        settings.color = lodestone_render::fog_color_for_time_of_day(time_of_day, fog.color);
        let mut u = FogUniform::new(&settings, eye);
        u.end_enabled[2] = sky_darken;
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

    /// Install the source for the local player's **main-hand item** (see
    /// [`MainHandSource`]), so first person draws the held item instead of a bare
    /// arm.
    ///
    /// Until installed, the bare arm is drawn unconditionally — vanilla's
    /// empty-hand branch — which is what this shell did before the item path
    /// existed. `f` returns the item id of the *selected hotbar slot*, or `None`
    /// for an empty hand.
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
    /// // builds for the HUD; take that record's item id.
    /// let held: Option<lodestone_assets::ResourceLocation> = None; // = hotbar[selected].item
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
    /// `Option<ResourceLocation>`, as the example above), which it already is for
    /// every caller.
    pub fn set_main_hand_source(
        &mut self,
        f: impl Fn() -> Option<lodestone_assets::ResourceLocation> + Send + Sync + 'static,
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
    /// **Nothing in this workspace calls this yet.** The gather it needs
    /// (`crate::block_entities::bell_spawns`) exists and is tested, but the
    /// per-frame install call site — the `sim.rs`/`app.rs` equivalent of
    /// [`Self::set_block_entity_source`]'s own installer — is outside this
    /// change's file ownership; see `docs/block-entity-renderers.md`'s Bell
    /// section for the exact remaining hop. Until installed, a bell draws
    /// nothing extra beyond its block model's own attachment-frame geometry,
    /// the same "unset means draw nothing" degradation every other source
    /// here has.
    pub fn set_bell_source(
        &mut self,
        f: impl Fn(Vec3) -> Vec<lodestone_render::BellSpawn> + Send + Sync + 'static,
    ) {
        self.bell_source = BellSource(Some(Box::new(f)));
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
