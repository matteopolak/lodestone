//! GPU render state for the shell: owns the block pipeline, the atlas, a depth
//! buffer, and a per-section table of uploaded meshes + camera uniforms, and
//! draws them all in one pass.
//!
//! The **packed** (demo-world) path still gives every section its own
//! camera-uniform buffer, rewritten with the current `view_proj` each frame
//! before the pass opens (see [`upload_packed_section`](RenderState::upload_packed_section)
//! and the top of [`render_inner`](RenderState::render_inner)) — the demo
//! world is capped at a few thousand sections and never runs live, so this was
//! never the measured cost.
//!
//! The **model** (live-vanilla) path does not: issue #75 profiled a live
//! session and found this same per-section-buffer shape responsible for 52.9%
//! of main-thread CPU, rewriting *every* resident section's whole camera
//! uniform every frame — thousands of `queue.write_buffer` calls for data that
//! is almost entirely constant (`view_proj` is identical for every section;
//! only `section_origin` differs, and it never changes for a section's
//! lifetime). [`ModelRenderer`] instead keeps one shared camera+fog buffer
//! (written once per frame) and one [`SectionOriginArena`] of per-section
//! origins addressed by a dynamic offset at draw time (written once, at
//! upload). See `docs/section-camera-uniform.md`.
use std::collections::HashMap;

use lodestone_assets::ResourceLocation;
use lodestone_assets::entity_models::sheep_wool_tint;
use lodestone_assets::equipment::{ArmourLayerType, ArmourSlot};
use lodestone_render::{
    BlockAtlas, BlockPipeline, Camera, CameraUniform, DepthBuffer, ENTITY_FULLBRIGHT,
    EntityCameraUniform, GpuAtlas, GpuMesh, GpuModelMesh, InstanceTint, ItemGeometry, Mesh,
    ModelMesh, ModelPipeline, SpriteAnimation,
    block::{camera_buffer, sprite_uv_buffer},
    crack_pipeline::{CrackPipeline, GpuCrackMesh},
    crack_resolver::CrackResolver,
    entity::{
        Arm, armour_layer_tint, armour_layers, camera_orientation, dropped_item_mesh,
        ground_transform, hand_transform, held_item_mesh, item_bob_offset, thrown_item_for,
        thrown_item_mesh,
    },
    fog::{FogSettings, FogUniform},
    model_anim_buffer, model_camera_buffer, model_shared_camera_buffer, plan_block_entities,
    plan_entities, update_model_anim_buffer, update_model_shared_camera_buffer, upload_instances,
    upload_instances_tinted,
    vertex::vram_bytes,
};

use glam::Vec3;

use lodestone_model::event::EquipmentSlot;

use crate::entities::{EntityDraw, ITEM_ENTITY_TYPE_PATH};
use crate::mesher::{SectionGeometry, SectionKey};
use crate::particles::{ParticleInstance, ParticleRenderer};

mod block_entities;
mod debug_lines;
mod entities;
mod first_person;
mod nametag;
mod outline;
mod screen_effects;
mod sources;
mod stats;
mod terrain;

pub use debug_lines::{DebugLineVertex, debug_line_vertices};
pub use outline::CrackTarget;
pub use screen_effects::ScreenEffects;
pub use sources::{
    BlockEntitySource, EntityLightSource, HandSwingSource, MainHandSource, OutlineShapeSource,
    SkyDarkenSource, ThirdPersonBodySource, ThirdPersonBodyState,
};
pub use stats::RenderStats;

use block_entities::{BlockEntityDrawBatch, BlockEntityRenderer};
use debug_lines::{DebugLineRenderer, DebugLinesSource};
use entities::EntityRenderer;
use first_person::FirstPersonHand;
use nametag::NameTagRenderer;
use outline::OutlineRenderer;
use sources::TimeOfDaySource;
use terrain::{
    MODEL_ORIGIN_ARENA_SLOTS, ModelRenderer, ModelSectionGpu, SectionGpu, SectionOriginArena,
    anim_slots_at,
};
#[cfg(test)]
use entities::{load_humanoid_armour_textures, model_tint};
#[cfg(test)]
use lodestone_render::{AnimInput, ArmourModelSet, EntityModelSet};
#[cfg(test)]
use sources::LOCAL_PLAYER_DRAW_ID;

/// The sky colour, in linear RGB.
///
/// Shared deliberately: this is both what the frame clears to *and* what
/// distance fog fades terrain into. If the two drifted apart the horizon would
/// show a band of haze in a colour the sky never is, so they read one constant.
///
/// This is `srgb_to_linear([0.53, 0.71, 0.92])` — `#87B5EB`, the intended
/// sky-blue hex, divided by 255 and then actually linearised. The constant
/// used to hold that `#87B5EB / 255` triple directly, labelled linear when it
/// was really sRGB; every consumer (this clear colour, and the fog colour in
/// `sim::fog_for_render_distance`) treats it as linear and gets gamma-encoded
/// again on the way to the screen, so the mislabelled value washed the sky out
/// (it displayed as `(192, 219, 246)`, saturation 0.22, instead of the intended
/// `(135, 181, 235)`).
///
/// This is the **bring-up default** only — `RenderState::new` seeds both the
/// clear colour and the fog colour from it, but `app.rs`'s `redraw()` then
/// drives both away from it together every frame a dimension-conditioned fog
/// (`Sim::fog_settings`, e.g. `FogSettings::nether`/`the_end`) or a submersion
/// override is active, via `RenderState::set_fog`/`set_clear_color` — always
/// called as a pair with the same colour, per `docs/dimension-visuals.md`.
pub const SKY_COLOR: [f32; 3] = [0.242_867, 0.462_361, 0.827_571];

/// Fraction of the view distance at which fog begins.
///
/// The outer quarter of the render volume is the fade band: near enough that
/// the edge chunks dissolve rather than pop in, far enough that fog is not
/// visible during normal play.
pub const FOG_START_FRACTION: f32 = 0.75;

/// Owns all GPU resources needed to render the world.
#[derive(Debug)]
pub struct RenderState {
    pipeline: BlockPipeline,
    #[allow(dead_code)]
    atlas: GpuAtlas,
    #[allow(dead_code)]
    uv_buffer: wgpu::Buffer,
    atlas_bind_group: wgpu::BindGroup,
    depth: DepthBuffer,
    sections: HashMap<SectionKey, SectionGpu>,
    model: Option<ModelRenderer>,
    outline: OutlineRenderer,
    /// The render half of `ExtractSet::Debug` (`docs/plugin-api.md`); see
    /// [`DebugLinesSource`] for why it starts empty until something installs
    /// a source.
    debug_lines: DebugLineRenderer,
    debug_lines_source: DebugLinesSource,
    entities: EntityRenderer,
    /// Block-break debris **and** sheet particles (flame, smoke, crits,
    /// splashes). Bound to *both* stitches: whichever atlas the terrain draws
    /// from, so a debris fragment is textured from the same pixels as the block
    /// it came off, and the separate particle sheet, so a flame is textured
    /// from `textures/particle/flame.png` — see [`Self::particle_sheet_atlas`].
    particles: ParticleRenderer,
    particle_atlas_bind_group: wgpu::BindGroup,
    /// The stitched particle sheet uploaded to the GPU, or `None` on a jar-less
    /// run (headless tests, no `client.jar`), in which case
    /// [`Self::particle_atlas_bind_group`] holds a 1×1 transparent stand-in in
    /// the sheet slots.
    ///
    /// Kept as a field, not dropped after building the bind group: `has_*`
    /// needs it, and issue #45's whole lesson is that "the sheet texture is
    /// installed" must be answerable from outside this module rather than
    /// inferred from pixels.
    particle_sheet_atlas: Option<GpuAtlas>,
    /// One-shot latch for the "sheet instances submitted, no sheet texture"
    /// warning in [`Self::prepare_particles`]. A per-frame log would be 60
    /// lines a second of the same sentence.
    warned_missing_particle_sheet: bool,
    /// What a pixel nothing else drew this frame clears to. Seeded from
    /// [`SKY_COLOR`] at construction; kept in step with [`fog`](Self::fog)'s
    /// colour thereafter via [`RenderState::set_clear_color`] — see that
    /// method's doc for why the two must never disagree.
    clear: wgpu::Color,
    /// Linear distance fog fading the outermost loaded chunks into the sky (or,
    /// later, a biome water colour when submerged). Defaults to a sky-coloured
    /// fog sized for the default render distance; drive it from the real render
    /// distance / eye-in-fluid state via [`RenderState::set_fog`].
    fog: FogSettings,
    /// How each mob's world light is sampled. Full-bright until the shell wires
    /// a real world in via [`RenderState::set_entity_light_source`].
    entity_light: EntityLightSource,
    /// How bright the sky is *right now*. Permanent noon until the shell wires a
    /// world clock in via [`RenderState::set_sky_darken_source`].
    sky_darken: SkyDarkenSource,
    /// Where the local player's own third-person body comes from, if a
    /// caller has wired one in. Unset until the shell has both a
    /// third-person camera and a way to describe the local player's pose —
    /// see [`RenderState::set_third_person_body_source`].
    third_person_body: ThirdPersonBodySource,
    /// How far through an arm swing the local player is *right now*. A rested
    /// arm until the shell wires its swing clock in via
    /// [`RenderState::set_hand_swing_source`].
    hand_swing: HandSwingSource,
    /// What the local player is holding in their main hand, for the first-person
    /// pass. Empty (bare arm) until the shell wires it in via
    /// [`RenderState::set_main_hand_source`].
    main_hand: MainHandSource,
    outline_shape: OutlineShapeSource,
    /// The sky pass (disc/sun/moon/stars/clouds), built once the vanilla
    /// celestial atlas and cloud texture are available. `None` — no
    /// `client.jar`, a headless test, or simply before [`RenderState::install_sky`]
    /// runs — reproduces this struct's behaviour before the sky existed
    /// exactly: [`render_inner`](Self::render_inner) clears straight to
    /// [`Self::clear`] and draws no sky pass at all.
    sky: Option<lodestone_render::SkyRenderer>,
    /// The world clock the sky pass reads (see [`TimeOfDaySource`]). Permanent
    /// noon until the shell wires a world clock in via
    /// [`RenderState::set_time_of_day_source`] — the same "unset means noon"
    /// convention [`SkyDarkenSource`] already uses.
    time_of_day: TimeOfDaySource,
    /// The underwater/fire screen-overlay pass (issues #108, #112), built once
    /// the vanilla `underwater.png`/`fire_1.png` textures are available. `None`
    /// — no `client.jar`, a headless test, or simply before
    /// [`RenderState::install_screen_effects`] runs — draws neither overlay,
    /// the same "no pass installed, nothing extra drawn" convention
    /// [`Self::sky`] uses.
    screen_effects: Option<lodestone_render::ScreenEffectRenderer>,
    /// Billboarded entity/player nametags (issue #100). Always constructed —
    /// unlike [`Self::sky`]/[`Self::screen_effects`], there is no "install"
    /// step: [`NameTagRenderer::new`] loads its own jar-sourced font
    /// (fail-open to drawing nothing, same contract as
    /// [`crate::hud::vanilla_font::VanillaFont::shared`]), so nothing
    /// downstream needs to know whether it succeeded.
    nametag: NameTagRenderer,
    /// Block-entity rigs — chests today (issue #23). Always constructed, like
    /// [`Self::nametag`] and unlike [`Self::sky`]: it loads its own sheets from
    /// the jar and fail-opens to drawing nothing, so there is no install step for
    /// a caller to forget.
    ///
    /// A chest has no block model whatsoever in 26.2, so without this the block
    /// pass leaves a **hole** where every chest is; that is why it is
    /// unconditional rather than opt-in.
    block_entities: BlockEntityRenderer,
    /// Where this frame's chests come from. Empty until the shell wires the
    /// world in via [`RenderState::set_block_entity_source`] — the same
    /// "unset means draw nothing" convention every other source here uses.
    block_entity_source: BlockEntitySource,
}

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
        let depth = DepthBuffer::new(device, width.max(1), height.max(1));
        let outline = OutlineRenderer::new(device, color_format);
        let debug_lines = DebugLineRenderer::new(device, color_format);
        let entities = EntityRenderer::new(device, queue, color_format);
        let nametag = NameTagRenderer::new(device, color_format);

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
            let items: HashMap<ResourceLocation, ItemGeometry> = models
                .items()
                .map(|(id, geometry)| (id.clone(), geometry.clone()))
                .collect();
            // The shared per-frame half of the section camera (view_proj +
            // fog) and the per-section origin arena (issue #75 — see the
            // module doc). One bind group over both, built once; every
            // section draw and the dropped-item pass reuse it, varying only
            // the dynamic offset.
            let origin_arena = SectionOriginArena::new(device, queue, MODEL_ORIGIN_ARENA_SLOTS);
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
            model,
            outline,
            debug_lines,
            debug_lines_source: DebugLinesSource::default(),
            entities,
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
            // A calm sky blue, so terrain reads clearly against it.
            clear: wgpu::Color {
                r: SKY_COLOR[0] as f64,
                g: SKY_COLOR[1] as f64,
                b: SKY_COLOR[2] as f64,
                a: 1.0,
            },
            // Fog fades into that same sky colour. Sized for the default 8-chunk
            // render distance; the shell overrides it from its real render
            // distance (and underwater state) via `set_fog`.
            fog: FogSettings::for_view_distance(SKY_COLOR, 8.0 * 16.0, FOG_START_FRACTION),
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
    pub fn set_fog(&mut self, fog: FogSettings) {
        self.fog = fog;
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
    /// This frame's fog uniform with the sky-darken factor folded into its spare
    /// lane, so **terrain and mobs read the same clock**. Wiring one without the
    /// other is worse than wiring neither: at midnight it makes mobs darker than
    /// the blocks they stand on, which reads as a mob-rendering bug rather than a
    /// missing feature.
    fn fog_with_clock(&self, eye: glam::Vec3) -> FogUniform {
        let mut fog = FogUniform::new(&self.fog, [eye.x, eye.y, eye.z]);
        fog.end_enabled[2] = self.sky_darken.value();
        fog
    }

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
    pub fn set_main_hand_source(
        &mut self,
        f: impl Fn() -> Option<lodestone_assets::ResourceLocation> + Send + Sync + 'static,
    ) {
        self.main_hand = MainHandSource(Some(Box::new(f)));
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

    /// Recreate the depth buffer to match a resized target.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.depth.width != width || self.depth.height != height {
            self.depth = DepthBuffer::new(device, width, height);
        }
    }

    /// Upload (or replace) a section's mesh. An empty mesh removes the section.
    ///
    /// Dispatches on the geometry variant: packed full-cube meshes (demo world)
    /// go to the packed [`BlockPipeline`] table; wide baked-model meshes (live
    /// vanilla world) go to the [`ModelRenderer`] table. A `Model` upload with no
    /// model renderer present (never happens in a consistent session, since the
    /// vanilla classifier and the model renderer are built from the same atlas)
    /// is a no-op.
    pub fn upload_section(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: SectionKey,
        mesh: &SectionGeometry,
    ) {
        match mesh {
            SectionGeometry::Packed(mesh) => self.upload_packed_section(device, key, mesh),
            SectionGeometry::Model { opaque, water } => {
                let Some(model) = self.model.as_mut() else {
                    return;
                };
                let origin = key.origin();
                let origin_f = [origin[0] as f32, origin[1] as f32, origin[2] as f32];
                let opaque_gpu = GpuModelMesh::upload(device, opaque);
                let water_gpu = GpuModelMesh::upload(device, water);
                // A remesh of an already-resident coord (the dirty-propagation
                // case) reuses that coord's origin slot rather than leaking it —
                // the origin is a pure function of `key`, so it never actually
                // changes.
                let existing = model.sections.remove(&key);
                // A section may carry only opaque terrain, only water (an ocean
                // surface section with no solid blocks), or both. Drop it only
                // when neither has geometry.
                if opaque_gpu.is_none() && water_gpu.is_none() {
                    if let Some(old) = existing {
                        model.origin_arena.free(old.origin_alloc);
                    }
                    return;
                }
                let origin_alloc = match existing {
                    Some(old) => old.origin_alloc,
                    None => match model.origin_arena.alloc(queue, origin_f) {
                        Some((alloc, _offset)) => alloc,
                        None => {
                            // Should not happen — see `SectionOriginArena`'s
                            // doc for the capacity margin — but degrade to a
                            // dropped (missing) section rather than a panic if
                            // it ever does.
                            tracing::warn!(
                                "section-origin arena exhausted at {key:?}; \
                                 dropping this section's geometry"
                            );
                            return;
                        }
                    },
                };
                model.sections.insert(
                    key,
                    ModelSectionGpu {
                        mesh: opaque_gpu,
                        quad_count: opaque.quad_count(),
                        water: water_gpu,
                        water_quad_count: water.quad_count(),
                        origin_alloc,
                    },
                );
            }
        }
    }

    /// Upload a packed full-cube section (the demo path).
    fn upload_packed_section(&mut self, device: &wgpu::Device, key: SectionKey, mesh: &Mesh) {
        match GpuMesh::upload(device, mesh) {
            None => {
                self.sections.remove(&key);
            }
            Some(gpu_mesh) => {
                let origin = key.origin();
                let origin_f = [origin[0] as f32, origin[1] as f32, origin[2] as f32];
                // Placeholder uniform; overwritten every frame with the live camera.
                let cam_buffer = camera_buffer(
                    device,
                    CameraUniform {
                        view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                        section_origin: [origin_f[0], origin_f[1], origin_f[2], 0.0],
                    },
                );
                let cam_bind_group = self.pipeline.camera_bind_group(device, &cam_buffer);
                self.sections.insert(
                    key,
                    SectionGpu {
                        mesh: gpu_mesh,
                        quad_count: mesh.quad_count(),
                        origin: origin_f,
                        cam_buffer,
                        cam_bind_group,
                    },
                );
            }
        }
    }

    /// Remove a section (e.g. an unloaded chunk).
    pub fn remove_section(&mut self, key: &SectionKey) {
        self.sections.remove(key);
        if let Some(model) = self.model.as_mut()
            && let Some(old) = model.sections.remove(key)
        {
            model.origin_arena.free(old.origin_alloc);
        }
    }

    /// Number of uploaded (non-empty) sections.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.sections.len() + self.model.as_ref().map_or(0, |m| m.sections.len())
    }

    /// The stitched **model** atlas's texture view — the atlas whose UVs every
    /// [`BakedQuad`](lodestone_assets::BakedQuad) indexes, terrain and 3-D item
    /// icons alike. `None` on the demo path, which has no baked models.
    ///
    /// Lent out (rather than re-uploaded) so a second consumer of the model
    /// shader — the HUD's 3-D item pass — samples the *same* GPU texture. `wgpu`
    /// resources are `Arc`-backed and a bind group keeps its own strong
    /// reference, so a caller may build a bind group from this borrow and outlive
    /// it. Uploading a second copy of the block atlas for the hotbar would cost
    /// tens of megabytes to draw nine 16 px icons.
    #[must_use]
    pub fn model_atlas_view(&self) -> Option<&wgpu::TextureView> {
        self.model.as_ref().map(|m| &m.atlas.view)
    }

    /// The model atlas's sampler, paired with [`Self::model_atlas_view`].
    #[must_use]
    pub fn model_atlas_sampler(&self) -> Option<&wgpu::Sampler> {
        self.model.as_ref().map(|m| &m.atlas.sampler)
    }

    /// The tint-palette uniform buffer the model shader reads at group 2. Shared
    /// so a hotbar icon's tinted faces (grass block, leaves) resolve through the
    /// same palette slots as the world block.
    #[must_use]
    pub fn model_palette_buffer(&self) -> Option<&wgpu::Buffer> {
        self.model.as_ref().map(|m| &m.palette_buffer)
    }

    /// The per-slot animation uniform buffer the model shader reads at group 3,
    /// rewritten every frame by [`update_animation`](Self::update_animation).
    ///
    /// Sharing it is what makes an animated **item** icon (magma block, sea
    /// lantern, prismarine) advance in lock-step with the same block in the
    /// world, for free: one buffer, one per-frame write, two readers.
    #[must_use]
    pub fn model_anim_buffer(&self) -> Option<&wgpu::Buffer> {
        self.model.as_ref().map(|m| &m.anim_buffer)
    }

    /// The depth attachment sized to the current target. Lent to the HUD's 3-D
    /// item pass, which needs a depth buffer for the near faces of an isometric
    /// mini-block to win over the far ones. That pass **clears** it, so it does
    /// not disturb the world depth already consumed earlier in the frame.
    #[must_use]
    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth.view
    }

    /// Total merged quads currently resident on the GPU.
    #[must_use]
    pub fn total_quads(&self) -> usize {
        let packed: usize = self.sections.values().map(|s| s.quad_count).sum();
        let model: usize = self
            .model
            .as_ref()
            .map_or(0, |m| m.sections.values().map(|s| s.quad_count).sum());
        packed + model
    }

    /// Render every section into `view` using `camera`. Writes all section
    /// camera uniforms first, then draws. If `outline` names a block, a
    /// wireframe box is drawn around it after the terrain.
    /// Rewrite the animated-block uniform for the current game `tick`.
    ///
    /// Call once per frame *before* [`render`](Self::render) with the live game
    /// tick (`Sim::tick_count`). Each animated sprite slot is sampled at `tick`
    /// via the existing `anim.rs` timing and its resolved V offset uploaded, so
    /// the model/fluid shaders draw the correct frame. A no-op when there is no
    /// live-vanilla model pass (the offline demo path). Skipping it leaves every
    /// sprite on frame 0 — the pre-wiring behaviour — rather than erroring.
    pub fn update_animation(&self, queue: &wgpu::Queue, tick: u64) {
        if let Some(model) = &self.model {
            let slots = anim_slots_at(&model.animations, tick);
            update_model_anim_buffer(queue, &model.anim_buffer, &slots);
        }
    }

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
            None,
            ScreenEffects::default(),
        )
    }

    /// Like [`render`](Self::render), but also draws the progressive mining-crack
    /// overlay on the target block. The crack follows the block's real model
    /// geometry (slabs/stairs/crosses), not a synthetic cube.
    pub fn render_with_crack(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        camera: &Camera,
        outline: Option<[i32; 3]>,
        entities: &[EntityDraw],
        crack: CrackTarget,
    ) -> RenderStats {
        self.render_inner(
            device,
            queue,
            view,
            camera,
            outline,
            entities,
            Some(crack),
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
            None,
            screen_effects,
        )
    }

    /// [`render_with_crack`](Self::render_with_crack) +
    /// [`render_with_effects`](Self::render_with_effects) together — the shape
    /// `app.rs`'s real per-frame call site needs (mining and the overlays are
    /// both possible at once).
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
        crack: CrackTarget,
        screen_effects: ScreenEffects,
    ) -> RenderStats {
        self.render_inner(
            device,
            queue,
            view,
            camera,
            outline,
            entities,
            Some(crack),
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
        crack: Option<CrackTarget>,
        screen_effects: ScreenEffects,
    ) -> RenderStats {
        let view_proj = camera.view_projection().to_cols_array_2d();

        // Rewrite each section's uniform with the current view-projection.
        for section in self.sections.values() {
            let uniform = CameraUniform {
                view_proj,
                section_origin: [section.origin[0], section.origin[1], section.origin[2], 0.0],
            };
            queue.write_buffer(&section.cam_buffer, 0, bytemuck::bytes_of(&uniform));
        }

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
            let eye = camera.position;
            let fog = self.fog_with_clock(eye);
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
        stats.first_person_item_drawn = matches!(first_person_hand, Some(FirstPersonHand::Item(_)));

        // Build the mining-crack overlay mesh before the pass (buffers can't be
        // created mid-pass). It follows the target block's real model geometry;
        // an air or unknown state, an out-of-range stage, or a block whose model
        // has no faces yields `None` and nothing is drawn. The crack camera uses
        // world-space positions (section origin zero), so rewrite its uniform
        // with the current view-projection.
        let crack_mesh = crack.and_then(|target| {
            let model = self.model.as_ref()?;
            let origin = [
                target.block[0] as f32,
                target.block[1] as f32,
                target.block[2] as f32,
            ];
            let mesh = model
                .crack_resolver
                .mesh_for(target.state_id, target.stage, origin)?;
            let gpu = GpuCrackMesh::upload(device, &mesh)?;
            queue.write_buffer(
                &model.crack_cam_buffer,
                0,
                bytemuck::bytes_of(&CameraUniform {
                    view_proj,
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                }),
            );
            Some(gpu)
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });

        // The sky pass, if installed — its own render pass with no depth
        // attachment, run *before* the block pass (`SkyRenderer::render`'s own
        // doc: it must run first and take no depth, so it can never occlude
        // terrain and terrain always draws over it normally). It clears the
        // target itself (`Color::BLACK`, overwritten by every pixel the four
        // sky draws touch), so the block pass below must switch from its own
        // `Clear` to a `Load` — clearing twice would just discard the sky.
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
            .with_void_fog(lodestone_render::fog::VoidFog::OVERWORLD);
            sky.render(device, queue, &mut encoder, view, camera, &frame);
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
                pass.set_bind_group(0, &section.cam_bind_group, &[]);
                pass.set_vertex_buffer(0, section.mesh.vertices.slice(..));
                pass.set_index_buffer(section.mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..section.mesh.index_count, 0, 0..1);
                stats.sections_drawn += 1;
                stats.draw_calls += 1;
                stats.total_quads += section.quad_count;
            }

            // Live vanilla terrain: wide baked-model geometry through the model
            // pipeline (cross-plants, slabs, stairs, tinted grass, cutout via the
            // shader's alpha discard). Shares the terrain depth buffer.
            if let Some(model) = &self.model {
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

                // Mining-crack overlay on the target block, drawn after the
                // opaque terrain it sits on (so the block face is already in the
                // depth buffer) and before translucent water. The pipeline's
                // negative depth bias pulls the crack toward the camera so its
                // `destroy_stage` texels win the depth test against the coplanar
                // face without z-fighting; alpha-blended, depth-write off.
                if let Some(crack) = &crack_mesh {
                    pass.set_pipeline(&model.crack_pipeline.pipeline);
                    pass.set_bind_group(0, &model.crack_cam_bind_group, &[]);
                    pass.set_bind_group(1, &model.crack_atlas_bind_group, &[]);
                    pass.set_vertex_buffer(0, crack.vertices.slice(..));
                    pass.set_index_buffer(crack.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..crack.index_count, 0, 0..1);
                    stats.draw_calls += 1;
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

        // The underwater/fire screen overlays (issues #108, #112): their own
        // `Load` passes (see `ScreenEffectRenderer::draw_underwater`/`draw_fire`'s
        // doc — they must not erase the world/hand just drawn), run last,
        // matching vanilla's own order (`GameRenderer.java:568-577`: the hand,
        // then `screenEffectRenderer.submit`, then the HUD/feature renderers —
        // this shell's HUD draws in a later, separate pass in `app.rs`). Gated
        // on first-person and not spectator, matching vanilla's
        // `isFirstPerson && !isSpectator` (`ScreenEffectRenderer.submit`); this
        // crate has no "sleeping" state yet, so that conjunct is omitted — see
        // `ScreenEffects::any_active`'s doc.
        if let Some(fx) = &self.screen_effects {
            let first_person = !stats.third_person_body_drawn;
            if screen_effects.any_active(first_person) {
                if screen_effects.eye_in_water {
                    let light = self.entity_light.sample(camera.position);
                    fx.draw_underwater(queue, &mut encoder, view, camera.yaw, camera.pitch, light);
                    stats.underwater_overlay_drawn = true;
                }
                if screen_effects.on_fire {
                    fx.draw_fire(queue, &mut encoder, view, screen_effects.tick);
                    stats.fire_overlay_drawn = true;
                }
            }
        }

        queue.submit(std::iter::once(encoder.finish()));

        stats.vram_bytes = vram_bytes(stats.total_quads);
        stats
    }

    /// Mesh this frame's **world item geometry** — dropped items *and* items in
    /// mobs' hands — into one world-space [`GpuModelMesh`], and rewrite the
    /// pass's camera uniform.
    ///
    /// Returns `None` — and draws nothing — when there is no vanilla model pass,
    /// or when nothing on screen resolves to baked item geometry. For a drop that
    /// last case is vanilla's own behaviour: `ItemEntityRenderer.submit` returns
    /// immediately on an empty stack, and so does `ItemInHandLayer` on an empty
    /// hand.
    ///
    /// # One mesh, not one per item
    ///
    /// Each item's placement (a drop's bob and spin, a held item's arm chain) is
    /// folded into its **vertex positions** by [`dropped_item_mesh`] /
    /// [`held_item_mesh`], so unlike the mobs there is no per-instance matrix to
    /// batch on and no shared geometry between two different items. Concatenating
    /// them into a single buffer is therefore both the simplest and the cheapest
    /// option: one upload and one draw call per frame however many items exist,
    /// versus one of each per item.
    ///
    /// # Why held items are here and not in the entity pass
    ///
    /// An item is an *item model* — the same baked quads a hotbar slot uses —
    /// not a cuboid part rig, so it cannot go through [`EntityPipeline`] however
    /// closely it is attached to one. The only thing the entity side contributes
    /// is the arm's world matrix, which is why this reads `part_transforms` out
    /// of a freshly resolved instance rather than the other way round.
    fn prepare_item_geometry(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Option<GpuModelMesh> {
        let model = self.model.as_ref()?;
        let frustum = camera.frustum();
        let mut combined = ModelMesh::default();
        // `camera.orientation` for every thrown projectile this frame: one
        // matrix, not one per entity — a billboard's rotation depends only on the
        // camera.
        let orientation = camera_orientation(camera.view_matrix());
        for draw in entities {
            if draw.type_path != ITEM_ENTITY_TYPE_PATH {
                if let Some(thrown) = thrown_item_for(&draw.type_path) {
                    self.merge_thrown_item(
                        model,
                        draw,
                        thrown,
                        orientation,
                        &frustum,
                        &mut combined,
                        stats,
                    );
                    // A projectile holds no equipment; skip the held-item scan.
                    continue;
                }
                self.merge_held_items(model, draw, &frustum, &mut combined, stats);
                continue;
            }
            // No stack reported (today: all of them — see
            // `EntityInterpolator::set_item_stack`) or a sprite-only item with
            // no 3-D geometry: draw nothing rather than a stand-in.
            let Some(geometry) = draw.item.as_ref().and_then(|id| model.items.get(id)) else {
                continue;
            };
            // A drop is at most a quarter-block across, so a cheap point-in-
            // frustum test on its position is enough to keep off-screen piles
            // out of the buffer without an AABB.
            if !frustum.intersects_aabb(
                draw.feet - glam::Vec3::splat(0.5),
                draw.feet + glam::Vec3::splat(0.5),
            ) {
                continue;
            }
            // The item's **own** `display.ground`, now that the asset layer
            // carries every slot and not just `gui`. `ground_transform` falls
            // back to the `GuiLight`-keyed vanilla constants only for a model
            // chain that declares no `ground` at all.
            let ground = ground_transform(&geometry.display, geometry.gui_light);
            combined.merge(&dropped_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                &ground,
                draw.feet,
                draw.anim.age_ticks,
                item_bob_offset(draw.id),
                self.entity_light.sample(draw.feet),
            ));
            stats.item_drops_drawn += 1;
        }
        let mesh = GpuModelMesh::upload(device, &combined)?;
        stats.total_quads += combined.quad_count();
        // No camera write here: dropped items draw through `model.cam_bind_group`,
        // the same shared view_proj+fog buffer every section uses, written once
        // per frame at the top of `render_inner` — not a buffer of their own.
        Some(mesh)
    }

    /// Merge one thrown item projectile into `combined` as a camera-facing
    /// billboard of its item model — vanilla's `ThrownItemRenderer`.
    ///
    /// # Which item id, and why the wire is preferred over the table
    ///
    /// `ThrowableItemProjectile`, `Fireball` and `EyeOfEnder` all sync their stack
    /// through `DATA_ITEM_STACK` — the **same** `ITEM_STACK` serializer at the same
    /// metadata index a dropped item uses, so `EntityDraw::item` is already
    /// populated for a projectile with no new plumbing (`apply_entity_metadata`
    /// inserts `DisplayItem` for any entity type, not just `item`). That value is
    /// authoritative and takes precedence.
    ///
    /// [`ThrownItem::item`] is the fallback for the case the wire cannot cover:
    /// vanilla only marks the field dirty when a constructor *sets* it, so a
    /// snowball thrown by a snow golem — built through the position-only
    /// constructor — arrives with the field never reported. Drawing nothing there
    /// would be a silent hole in exactly the situation a player is being pelted.
    ///
    /// # Full-bright
    ///
    /// [`ThrownItem::full_bright`] is vanilla's `getBlockLightLevel` override
    /// returning `15`; it maps onto [`ENTITY_FULLBRIGHT`], the same byte the GUI
    /// item path nails every vertex to. The world sample is used otherwise, so a
    /// snowball crossing a shadow dims and a fireball does not.
    fn merge_thrown_item(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        thrown: lodestone_render::entity::ThrownItem,
        orientation: glam::Mat4,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        // The wire's stack first, the registration's default second. `and_then`
        // rather than `or_else` on the geometry lookup: an id that resolves to no
        // baked geometry should fall through to the default too, not draw nothing.
        let geometry = draw
            .item
            .as_ref()
            .and_then(|id| model.items.get(id))
            .or_else(|| {
                let id: lodestone_assets::ResourceLocation = thrown.item.parse().ok()?;
                model.items.get(&id)
            });
        let Some(geometry) = geometry else {
            return;
        };
        // Scaled slack: a `fireball` is drawn at 3x, so a half-block box would cull
        // it while a third of it was still on screen.
        let slack = glam::Vec3::splat(0.5 * thrown.scale.max(1.0));
        if !frustum.intersects_aabb(draw.feet - slack, draw.feet + slack) {
            return;
        }
        let light = if thrown.full_bright {
            ENTITY_FULLBRIGHT
        } else {
            self.entity_light.sample(draw.feet)
        };
        // `display.ground`: `extractRenderState` resolves the item in
        // `ItemDisplayContext.GROUND`, the same context a drop uses — which is why
        // this is `ground_transform` and not a projectile-specific transform.
        let ground = ground_transform(&geometry.display, geometry.gui_light);
        combined.merge(&thrown_item_mesh(
            &geometry.quads,
            geometry.gui_light,
            &ground,
            draw.feet,
            orientation,
            thrown.scale,
            light,
        ));
        stats.projectiles_drawn += 1;
    }

    /// Merge whatever `draw` is holding into `combined`, posed off its own arm.
    ///
    /// Called for every non-item entity, so the early returns are the common
    /// path: most mobs carry no equipment at all, and `EntityDraw::equipment` is
    /// then an empty `Vec` and this costs one branch.
    ///
    /// # What is deliberately not handled
    ///
    /// * **The four humanoid armour slots.** Still skipped *here*, because armour
    ///   is not an item model hung off an arm — it is a cuboid mesh layer over
    ///   the wearer's rig, and it goes through the *entity* pipeline. See
    ///   [`RenderState::prepare_armour`], which is where `Head`/`Chest`/`Legs`/
    ///   `Feet` are consumed. Faking one here by posing an *item* model at a
    ///   chest slot would draw a floating chestplate icon, which is worse than
    ///   nothing.
    /// * **`Body` and `Saddle`.** Neither is humanoid armour and neither has a
    ///   mesh: `BODY` is `ANIMAL_ARMOR` (wolf armour, horse barding —
    ///   `WolfArmorLayer`, `HorseArmorLayer`) and `SADDLE` is its own type with
    ///   eleven per-mount layer types. See [`humanoid_armour_slot`] for why
    ///   folding `Body` into `Chest` is specifically wrong.
    /// * **Rigs with no arm.** A creeper with a `MainHand` item (a plugin can do
    ///   this) resolves no `right_arm` part, so nothing is drawn. Vanilla agrees:
    ///   `ItemInHandLayer` is only attached to renderers whose model implements
    ///   `ArmedModel`.
    fn merge_held_items(
        &self,
        model: &ModelRenderer,
        draw: &EntityDraw,
        frustum: &lodestone_render::Frustum,
        combined: &mut ModelMesh,
        stats: &mut RenderStats,
    ) {
        if draw.equipment.is_empty() {
            return;
        }
        // Cull on the holder, before doing any pose work: a mob behind the
        // camera cannot show its sword. Two blocks of slack around the feet
        // covers a tall mob plus the item's own reach.
        if !frustum.intersects_aabb(
            draw.feet - glam::Vec3::new(1.0, 0.5, 1.0),
            draw.feet + glam::Vec3::new(1.0, 2.5, 1.0),
        ) {
            return;
        }
        // The arm matrices come from the same resolver — and therefore the same
        // `AnimInput` — that `prepare_entities` puts on screen, so a held item can
        // never be posed off a different pose than the arm the player sees. An
        // entity type with no ported model resolves to `None` and holds nothing,
        // which is also what happens to the mob itself.
        let Some(instance) = self.entities.models.resolve(
            &draw.type_path,
            draw.feet,
            draw.yaw,
            draw.scale,
            &draw.anim,
        ) else {
            return;
        };
        let Some(mesh) = self.entities.models.get(instance.model) else {
            return;
        };
        // `net::entity_snapshot` maps `baby` onto a 0.5 uniform scale, which is
        // the only baby signal that reaches this layer — the same test
        // `entities.rs` already uses to pick `BABY_LIMB_SCALE`.
        let baby = draw.scale < 1.0;
        let light = self.entity_light.sample(draw.feet);

        for (slot, id) in &draw.equipment {
            // Every `Mob` returns `HumanoidArm.RIGHT` from `getMainArm()` (only
            // a `Player` can be left-handed, and the wire never tells us), so
            // main hand → right arm, off hand → left arm.
            let arm = match slot {
                EquipmentSlot::MainHand => Arm::Right,
                EquipmentSlot::OffHand => Arm::Left,
                // Humanoid armour is drawn by `prepare_armour` through the
                // entity pipeline; `Body`/`Saddle` are animal equipment with no
                // mesh at all. See this method's docs.
                _ => continue,
            };
            let Some(geometry) = model.items.get(id) else {
                continue;
            };
            // Prefer the dedicated hand transform over `part_transforms[arm]`.
            // Five models (skeleton/stray/wither_skeleton, player_slim, vex,
            // allay) shift or scale the item's pivot relative to the arm, and
            // that shift must *not* move the arm's own visible mesh — which is
            // what `part_transforms` places. `hand_transform` is exactly the
            // structural pose for every other model, so this is not a special
            // case, it is the correct source.
            let Some(arm_transform) = instance.hand_transform(arm).or_else(|| {
                let part = mesh.skeleton.index_of(arm.part_name())?;
                instance.part_transforms.get(part).copied()
            }) else {
                continue;
            };
            let transform = hand_transform(&geometry.display, arm, false);
            combined.merge(&held_item_mesh(
                &geometry.quads,
                geometry.gui_light,
                arm_transform,
                arm,
                baby,
                &transform,
                light,
            ));
            stats.held_items_drawn += 1;
        }
    }

    /// Resolve each interpolated entity into a renderable instance, frustum-cull
    /// and group them by model, upload one instance buffer per surviving model,
    /// and record draw/cull counts. Runs before the render pass so every GPU
    /// buffer it creates outlives the pass that reads it.
    ///
    /// # Why this plans twice (issue #98's hurt overlay)
    ///
    /// `plan_entities` groups by model and drops the input order, so a
    /// per-entity flag cannot be zipped back onto a batch afterwards — and
    /// `EntityInstance` (in `lodestone-render`'s `entity.rs`) carries only the
    /// light byte, not the overlay. The instances are therefore split by
    /// [`EntityDraw::hurt`] *before* planning, and each half's flag stays
    /// attached to the plan it produced as a `(bool, EntityFrame)` pair. That
    /// pairing is the point: a `Vec<bool>` parallel to the batches would be an
    /// invariant nothing enforces, which is precisely how this class of bug
    /// comes back. Grouping by `(model, hurt)` instead of `model` is also what
    /// a hurt mob costs in vanilla — one extra batch while its 10 ticks run,
    /// and nothing at all the rest of the time (the hurt half is empty, and
    /// `plan_entities` on an empty slice returns no batches).
    fn prepare_entities(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<EntityDrawBatch> {
        if entities.is_empty() {
            return Vec::new();
        }

        // Rewrite the entity group-0 uniform: view-projection (world position
        // lives per-instance, so the section origin stays zero), **this frame's
        // fog** from the same `self.fog` the terrain sections get, and **this
        // frame's sky darkening**. Both passes therefore fade on one curve; a mob
        // under water or at the render edge dissolves with the blocks around it
        // instead of punching through.
        //
        // Sky darkening rides the fog block's one spare lane, and is rewritten
        // every frame rather than at install time, because the world clock moves:
        // a value captured once would freeze the mob at whatever time of day it
        // happened to spawn.
        let eye = camera.position;
        queue.write_buffer(
            &self.entities.cam_buffer,
            0,
            bytemuck::bytes_of(
                &EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: camera.view_projection().to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: self.fog_with_clock(eye),
                }
                .with_sky_darken(self.sky_darken.value()),
            ),
        );

        // Split by `hurt` here, at the one point that still knows which
        // `EntityDraw` each instance came from.
        let mut plain: Vec<_> = Vec::new();
        let mut hurt: Vec<_> = Vec::new();
        for e in entities {
            // `resolve_posed`, not `resolve`, and this is the *only* call site that
            // needs it (issue #380): the pitch selects the **placement**, and a
            // projectile placed by the mob matrix draws 1.501 blocks high and
            // mirrored. For every mob the extra argument changes nothing — a mob's
            // pitch is head tracking and arrives through `e.anim`, not through the
            // placement — so the other five `resolve` call sites are deliberately
            // left alone rather than widened for symmetry.
            let Some(instance) = self
                .entities
                .models
                .resolve_posed(&e.type_path, e.feet, e.yaw, e.pitch, e.scale, &e.anim)
                .map(|i| i.with_light(self.entity_light.sample(e.feet)))
            else {
                continue;
            };
            if e.hurt { &mut hurt } else { &mut plain }.push(instance);
        }

        let frustum = camera.frustum();
        // The flag and the plan it describes travel as one value from here on.
        let plans = [
            (false, plan_entities(&plain, &frustum)),
            (true, plan_entities(&hurt, &frustum)),
        ];
        stats.entities_drawn = plans.iter().map(|(_, f)| f.stats.drawn).sum();
        stats.entities_culled = plans.iter().map(|(_, f)| f.stats.culled_frustum).sum();

        // One instance buffer per *part*, not per entity: the mesh's vertices are
        // part-local, so a limb only moves if its own matrices are uploaded
        // separately. A mob is ~10–35 parts but hundreds of quads, so this moves
        // roughly 1% of the data a per-entity vertex re-bake would.
        plans
            .iter()
            .flat_map(|(hurt, frame)| frame.batches.iter().map(move |batch| (*hurt, batch)))
            .map(|(hurt, batch)| {
                let count = u32::try_from(batch.transforms.len()).unwrap_or(u32::MAX);
                // Every instance in this batch shares one overlay state, by
                // construction of the split above — so one repeated value rather
                // than a per-instance vector, and no way for the two to disagree.
                let tints = vec![InstanceTint::NONE.with_hurt(hurt); batch.transforms.len()];
                // Every part uploads the *same* light and tint slices: a mob's
                // lightmap sample and its overlay state are per entity, so its
                // head and its leg share both values.
                let parts = batch
                    .parts
                    .iter()
                    .map(|p| upload_instances_tinted(device, p, &batch.lights, &tints))
                    .collect();
                EntityDrawBatch {
                    model: batch.model,
                    count,
                    parts,
                }
            })
            .collect()
    }

    /// Resolve this frame's **humanoid armour layers** into per-`(slot, texture)`
    /// instance buffers, ready to draw over the mobs wearing them.
    ///
    /// # Every piece is posed off the wearer's own part matrix
    ///
    /// Vanilla's armour model is an instance of the wearer's model *class* and is
    /// animated by the wearer's render state, so a zombie's chestplate reaches
    /// out in front with `animateZombieArms`. The equivalent here is to run no
    /// second pose at all: `ArmourMesh::attach` pairs each armour part with the
    /// wearer's index for the same name, and this reads
    /// `instance.part_transforms[i]` — the matrix the mob is *already* being
    /// drawn with.
    ///
    /// **Nothing is written back.** That is the same discipline
    /// `EntityInstance::hand_transforms` exists to enforce for held items: there,
    /// folding the item's pivot shift into `part_transforms` would have dragged
    /// the visible arm along with the sword. Armour needs the wearer's matrix
    /// *unmodified*, so there is nothing to fold in — but the rule is the same
    /// one, and a future "optimisation" that poses armour by mutating the
    /// wearer's transforms would break the mob, not the armour.
    ///
    /// # What is deliberately not handled
    ///
    /// * **Trims** (`minecraft:trim`). Not decoded anywhere in this engine and
    ///   not carried past `net::entity_snapshot`, so there is no input; they also
    ///   need a stitched trim-sprite atlas and a third depth mode
    ///   (`CompareOp.EQUAL`, no depth write). See `docs/armour-rendering.md`.
    /// * **A stack's own dye** (`minecraft:dyed_color`). Same reason: the
    ///   component is dropped at `entity_snapshot`, which narrows a stack to its
    ///   item id. Leather therefore always draws at
    ///   `Dyeable.colorWhenUndyed`, which is the correct answer for an undyed
    ///   piece and the only reachable one for a dyed one.
    /// * **Baby rigs.** Vanilla swaps in a whole second mesh set
    ///   (`createBabyArmorMesh`, `humanoid_baby` sheets, its own deformations);
    ///   a baby zombie wears adult armour scaled by the mob's 0.5 uniform scale
    ///   instead. Visibly close, not vanilla.
    /// * **Enchantment glint.** `hasFoil` is not on this side of the wire.
    fn prepare_armour(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<ArmourDrawBatch> {
        // No pack, no sheets, nothing to draw — and no synthetic fallback, on
        // purpose (see `EntityRenderer::armour_textures`).
        if self.entities.armour_textures.is_empty() {
            return Vec::new();
        }
        let frustum = camera.frustum();
        let mut accum: Vec<ArmourAccum> = Vec::new();

        for draw in entities {
            if draw.equipment.is_empty() {
                continue;
            }
            // Cheap reject before any pose work: most equipment is a held item.
            if !draw
                .equipment
                .iter()
                .any(|(slot, _)| humanoid_armour_slot(*slot).is_some())
            {
                continue;
            }
            // Same resolver, same `AnimInput` as `prepare_entities`, so a piece
            // of armour can never be posed off a different pose than the body it
            // is drawn over.
            let Some(instance) = self.entities.models.resolve(
                &draw.type_path,
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let Some(wearer) = self.entities.models.get(instance.model) else {
                continue;
            };
            let light = u32::from(self.entity_light.sample(draw.feet));

            // Walk the *slots* rather than the equipment list, so the draw order
            // is `HumanoidArmorLayer.submit`'s (chest, legs, feet, head)
            // regardless of what order the server happened to send.
            for slot in ArmourSlot::ALL {
                let Some((_, id)) = draw
                    .equipment
                    .iter()
                    .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))
                else {
                    continue;
                };
                // A modded namespace has no entry in the 26.2 asset table, and
                // guessing one would draw the wrong material.
                if id.namespace() != "minecraft" {
                    continue;
                }
                let layers = armour_layers(slot, id.path());
                if layers.is_empty() {
                    continue;
                }
                let Some(mesh) = self.entities.armour_models.get(slot) else {
                    continue;
                };
                // The humanoid gate lives inside `attach`: a pig handed a
                // chestplate resolves `body` by name and still wears nothing.
                let attached: Vec<_> = mesh.attach(&wearer.skeleton).collect();
                if attached.is_empty() {
                    continue;
                }
                for layer in layers {
                    let texture = (layer.texture, slot.layer_type());
                    if !self.entities.armour_textures.contains_key(&texture) {
                        continue;
                    }
                    // Vanilla's overlay is sampled by every layer of a
                    // `LivingEntityRenderer`'s model, armour included — a hurt
                    // mob whose breastplate stayed its own colour would read as
                    // a rendering fault, not as damage.
                    let tint = InstanceTint::rgb(armour_layer_tint(layer)).with_hurt(draw.hurt);
                    let group = match accum
                        .iter_mut()
                        .position(|a| a.slot == slot && a.texture == texture)
                    {
                        Some(i) => &mut accum[i],
                        None => {
                            accum.push(ArmourAccum {
                                slot,
                                texture,
                                parts: Vec::new(),
                            });
                            accum.last_mut().expect("just pushed")
                        }
                    };
                    for (range, wearer_index) in &attached {
                        let Some(transform) = instance.part_transforms.get(*wearer_index) else {
                            continue;
                        };
                        let part = match group.parts.iter_mut().position(|p| p.range == *range) {
                            Some(i) => &mut group.parts[i],
                            None => {
                                group.parts.push(ArmourPartAccum {
                                    range: *range,
                                    transforms: Vec::new(),
                                    lights: Vec::new(),
                                    tints: Vec::new(),
                                });
                                group.parts.last_mut().expect("just pushed")
                            }
                        };
                        part.transforms.push(*transform);
                        part.lights.push(light);
                        part.tints.push(tint);
                    }
                    stats.armour_layers_drawn += 1;
                }
            }
        }

        accum
            .into_iter()
            .map(|group| ArmourDrawBatch {
                slot: group.slot,
                texture: group.texture,
                parts: group
                    .parts
                    .into_iter()
                    .filter_map(|p| {
                        let count = u32::try_from(p.transforms.len()).unwrap_or(u32::MAX);
                        upload_instances_tinted(device, &p.transforms, &p.lights, &p.tints)
                            .map(|buffer| (p.range, buffer, count))
                    })
                    .collect(),
            })
            .collect()
    }

    /// Sheep wool layers (issue #53), over the same instances `prepare_entities`
    /// resolved — same resolver, same `AnimInput`, so wool can never be posed
    /// off a different pose than the body it grows out of. Mirrors
    /// [`prepare_armour`](Self::prepare_armour) exactly, minus the per-slot/
    /// per-texture grouping armour needs: wool has one mesh and one sheet, so
    /// every attached part accumulates into a single set of per-part buffers.
    ///
    /// # What is deliberately not handled
    ///
    /// * **Sheared sheep.** `draw.wool.sheared` is checked here, not filtered
    ///   upstream — [`EntityDraw::wool`]'s own doc explains why the data stays
    ///   honest about what the server reported. This is vanilla's own
    ///   `if (!state.isSheared)` gate (`SheepWoolLayer.submit`), applied at
    ///   exactly the point that draws the mesh.
    /// * **The pig/cow trap.** [`WoolMesh::attach`]'s `wearer_model` argument
    ///   is `instance.model` — the *resolved* model name — never
    ///   `wearer.family()`. `AnimFamily::Quadruped` is shared by `pig`, `cow`
    ///   and `wolf`; gating on family alone would grow wool on a pig the way
    ///   an ungated armour attach once drew a breastplate on one. In practice
    ///   `EntityDraw::wool` is already `None` for every non-sheep type
    ///   ([`crate::entities::sheep_wool`]'s own gate), so this is a second,
    ///   independent gate rather than the only one — belt and braces, the same
    ///   discipline `docs/entity-rendering.md` asks for.
    /// * **Baby sheep, the `jeb_` rainbow name, and the undercoat overlay.**
    ///   Not built — see `docs/entity-rendering.md`'s "deliberately out of
    ///   scope" list, unchanged by this pass.
    fn prepare_wool(
        &self,
        device: &wgpu::Device,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Vec<(lodestone_render::PartRange, wgpu::Buffer, u32)> {
        // No pack, no sheet, nothing to draw — and no synthetic fallback, on
        // purpose (see `EntityRenderer::wool_texture`).
        let (Some(wool_texture), Some(_wool_gpu)) =
            (&self.entities.wool_texture, &self.entities.wool_gpu)
        else {
            return Vec::new();
        };
        let _ = wool_texture; // presence check only; the bind group is read at draw time.
        let frustum = camera.frustum();
        let mut accum: Vec<WoolPartAccum> = Vec::new();

        for draw in entities {
            let Some(wool) = draw.wool else { continue };
            // Vanilla's own gate: a sheared sheep grows no wool mesh at all.
            if wool.sheared {
                continue;
            }
            let Some(instance) = self.entities.models.resolve(
                &draw.type_path,
                draw.feet,
                draw.yaw,
                draw.scale,
                &draw.anim,
            ) else {
                continue;
            };
            if !frustum.intersects_aabb(instance.aabb_min, instance.aabb_max) {
                continue;
            }
            let Some(wearer) = self.entities.models.get(instance.model) else {
                continue;
            };
            // The pig/cow-trap gate lives inside `attach`, keyed on the
            // resolved model name — see this method's docs.
            let attached: Vec<_> = self
                .entities
                .wool_models
                .mesh()
                .attach(&wearer.skeleton, instance.model)
                .collect();
            if attached.is_empty() {
                continue;
            }
            let light = u32::from(self.entity_light.sample(draw.feet));
            // Same reason armour carries it: the wool is one of the sheep's
            // model layers, so it reddens with the body.
            let tint = InstanceTint::rgb(sheep_wool_tint(wool.color)).with_hurt(draw.hurt);
            for (range, wearer_index) in &attached {
                let Some(transform) = instance.part_transforms.get(*wearer_index) else {
                    continue;
                };
                let part = match accum.iter_mut().position(|p| p.range == *range) {
                    Some(i) => &mut accum[i],
                    None => {
                        accum.push(WoolPartAccum {
                            range: *range,
                            transforms: Vec::new(),
                            lights: Vec::new(),
                            tints: Vec::new(),
                        });
                        accum.last_mut().expect("just pushed")
                    }
                };
                part.transforms.push(*transform);
                part.lights.push(light);
                part.tints.push(tint);
            }
            stats.wool_layers_drawn += 1;
        }

        accum
            .into_iter()
            .filter_map(|p| {
                let count = u32::try_from(p.transforms.len()).unwrap_or(u32::MAX);
                upload_instances_tinted(device, &p.transforms, &p.lights, &p.tints)
                    .map(|buffer| (p.range, buffer, count))
            })
            .collect()
    }

    /// Resolve this frame's block entities (chests, issue #23) into per-part
    /// instance buffers, frustum-culled and batched by `(model, sheet)`.
    ///
    /// # The one thing that is *not* like `prepare_entities`
    ///
    /// A chest's input does not come from the `entities` slice — it is a block,
    /// gathered from the world's decoded block-entity records by the source the
    /// shell installs. Everything downstream (per-part instance buffers, the
    /// group-0 camera+fog write, the `Frustum` cull) is deliberately identical,
    /// because a chest that fogged or lit differently from the mobs standing next
    /// to it would be the more visible bug.
    ///
    /// Light arrives already sampled on each [`lodestone_render::ChestSpawn`]
    /// rather than being read through [`Self::entity_light`] here: the gather
    /// already holds the world open to find the chest at all, and sampling there
    /// costs one lock instead of one per chest.
    fn prepare_block_entities(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        stats: &mut RenderStats,
    ) -> Vec<BlockEntityDrawBatch> {
        // Always reported, even on an empty frame: this is what separates "no
        // chests in view" from "no pack, so nothing can ever draw" — a chest with
        // no sheet draws nothing rather than a placeholder.
        stats.block_entity_sheets_loaded = self.block_entities.sheet_count();

        let eye = camera.position;
        let chests = self.block_entity_source.chests(eye);
        if chests.is_empty() {
            return Vec::new();
        }

        // Same group-0 contents as the entity pass, written to this pass's own
        // buffer: view-projection (world position is per-instance, so the section
        // origin stays zero), this frame's fog, and this frame's sky darkening.
        queue.write_buffer(
            &self.block_entities.cam_buffer,
            0,
            bytemuck::bytes_of(
                &EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: camera.view_projection().to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: self.fog_with_clock(eye),
                }
                .with_sky_darken(self.sky_darken.value()),
            ),
        );

        let instances: Vec<_> = chests
            .iter()
            .filter_map(|spawn| self.block_entities.models.resolve_chest(spawn))
            .collect();

        let frame = plan_block_entities(&instances, &camera.frustum());
        stats.block_entities_drawn = frame.stats.drawn;
        stats.block_entities_culled = frame.stats.culled_frustum;

        frame
            .batches
            .iter()
            .map(|batch| BlockEntityDrawBatch {
                model: batch.model,
                texture: batch.texture,
                count: batch.count(),
                // One buffer per part, for the reason `prepare_entities` gives:
                // vertices are part-local, so the lid only moves if its own
                // matrices are uploaded separately from the bottom's.
                parts: batch
                    .parts
                    .iter()
                    .map(|p| upload_instances(device, p, &batch.lights))
                    .collect(),
            })
            .collect()
    }
}

/// Per-part instance accumulation for the sheep wool layer, before upload.
/// Mirrors [`ArmourPartAccum`], minus the texture grouping: wool has one
/// sheet, so there is nothing to group by beyond the part itself.
struct WoolPartAccum {
    range: lodestone_render::PartRange,
    transforms: Vec<glam::Mat4>,
    lights: Vec<u32>,
    tints: Vec<InstanceTint>,
}

/// One model type's uploaded per-part instance buffers for a frame. `parts[p]`
/// holds one matrix per visible instance of part `p`; a `None` slot is a part
/// with no geometry (nothing to draw).
#[derive(Debug)]
struct EntityDrawBatch {
    model: &'static str,
    count: u32,
    parts: Vec<Option<wgpu::Buffer>>,
}

/// One `(armour slot, texture)` group's uploaded instance buffers for a frame.
///
/// The **order of these in the returned `Vec` is load bearing**: leather's
/// `humanoid` layer list is a dyeable base sheet and an untinted
/// `leather_overlay` at the *same* inflation, so the two are coplanar and the
/// overlay only wins the (`LessEqual`) depth test if it is drawn second. Batches
/// are accumulated in insertion order — slot in `ArmourSlot::ALL` order, then
/// layer in declaration order — never through a `HashMap`.
#[derive(Debug)]
struct ArmourDrawBatch {
    slot: ArmourSlot,
    texture: (&'static str, ArmourLayerType),
    /// `(index range, instance buffer, instance count)` per armour part that
    /// anything in this group used.
    parts: Vec<(lodestone_render::PartRange, wgpu::Buffer, u32)>,
}

/// Per-part instance accumulation for one `(slot, texture)` group, before upload.
struct ArmourAccum {
    slot: ArmourSlot,
    texture: (&'static str, ArmourLayerType),
    parts: Vec<ArmourPartAccum>,
}

struct ArmourPartAccum {
    range: lodestone_render::PartRange,
    transforms: Vec<glam::Mat4>,
    lights: Vec<u32>,
    tints: Vec<InstanceTint>,
}

/// The [`ArmourSlot`] an [`EquipmentSlot`] maps onto, or `None`.
///
/// This is vanilla's `EquipmentSlot.Type.HUMANOID_ARMOR` predicate
/// (`EquipmentSlot.java:15-19`) and nothing looser. In particular:
///
/// * **`Body` is not `Chest`.** `BODY` is `ANIMAL_ARMOR` — wolf armour and horse
///   barding live there — and `SADDLE` is its own type. A fold of `"body"` into
///   the chest slot was removed from the item census for exactly this reason
///   (`docs/item-prototypes.md`), and reintroducing it here would put a horse's
///   diamond barding on a player's torso.
/// * **`EquipmentSlot::isArmor` is the wrong predicate** even though it sounds
///   right: it is the *union* of humanoid and animal armour
///   (`EquipmentSlot.java:73-75`).
/// * `MainHand`/`OffHand` are held items and go through `merge_held_items`.
fn humanoid_armour_slot(slot: EquipmentSlot) -> Option<ArmourSlot> {
    match slot {
        EquipmentSlot::Head => Some(ArmourSlot::Head),
        EquipmentSlot::Chest => Some(ArmourSlot::Chest),
        EquipmentSlot::Legs => Some(ArmourSlot::Legs),
        EquipmentSlot::Feet => Some(ArmourSlot::Feet),
        EquipmentSlot::MainHand
        | EquipmentSlot::OffHand
        | EquipmentSlot::Body
        | EquipmentSlot::Saddle => None,
    }
}

/// A 1×1 fully transparent [`GpuAtlas`], for a texture slot whose real contents
/// are not available yet.
///
/// Used for the particle pass's sheet slot before
/// [`RenderState::install_particle_sheet_atlas`] runs. Transparent rather than
/// magenta-or-similar on purpose: the particle shader discards below `a < 0.02`,
/// so an unbacked sheet particle disappears instead of painting a debug colour
/// over the world. The *loud* half of that pairing is the one-shot warning in
/// [`RenderState::prepare_particles`] plus
/// [`RenderState::has_particle_sheet_atlas`] — a silent placeholder with no way
/// to observe it would be the island pattern again.
fn transparent_placeholder_atlas(device: &wgpu::Device, queue: &wgpu::Queue) -> GpuAtlas {
    GpuAtlas::from_rgba(device, queue, 1, 1, &[0, 0, 0, 0], &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_render::{HeadlessTarget, RenderTarget};

    /// The bytes the sky clear actually lands on in these tests' readbacks.
    ///
    /// Every headless test here uses an **`Rgba8Unorm`** target, so no gamma
    /// encode happens on write and the readback is [`SKY_COLOR`] (which is
    /// linear) scaled straight to bytes — *not* the `#87B5EB` the player sees on
    /// the sRGB swapchain.
    ///
    /// Derived rather than hardcoded because it was hardcoded twice and both
    /// copies went stale: when `SKY_COLOR` was corrected from a mislabelled sRGB
    /// triple to its true linear value, one of the three copies was updated and
    /// two were not. Those two tests then classified *every* pixel in the frame
    /// as "mob" — including the corners, which contain no mob — so their
    /// silhouette assertions were measuring the whole frame.
    #[must_use]
    fn sky_clear_bytes() -> [u8; 3] {
        SKY_COLOR.map(|c| (c * 255.0).round() as u8)
    }

    /// `Body` and `Saddle` must never reach the humanoid armour path, and the
    /// four that must are mapped exactly once each.
    ///
    /// This is the mapping a fold of `"body"` into `Chest` would break, and it
    /// has already been shipped wrong once on the census side — wolf armour and
    /// horse barding both live in `Body`, so the visible symptom is a player
    /// wearing a horse's diamond barding as a chestplate.
    #[test]
    fn only_the_four_humanoid_slots_map_to_armour() {
        use lodestone_assets::equipment::ArmourSlot;

        assert_eq!(
            humanoid_armour_slot(EquipmentSlot::Head),
            Some(ArmourSlot::Head)
        );
        assert_eq!(
            humanoid_armour_slot(EquipmentSlot::Chest),
            Some(ArmourSlot::Chest)
        );
        assert_eq!(
            humanoid_armour_slot(EquipmentSlot::Legs),
            Some(ArmourSlot::Legs)
        );
        assert_eq!(
            humanoid_armour_slot(EquipmentSlot::Feet),
            Some(ArmourSlot::Feet)
        );
        for slot in [
            EquipmentSlot::Body,
            EquipmentSlot::Saddle,
            EquipmentSlot::MainHand,
            EquipmentSlot::OffHand,
        ] {
            assert_eq!(
                humanoid_armour_slot(slot),
                None,
                "{slot:?} is not HUMANOID_ARMOR"
            );
        }
        // Every slot the model layer knows about is accounted for, so a new
        // vanilla slot fails here rather than being silently ignored.
        let mapped = EquipmentSlot::ALL
            .iter()
            .filter(|s| humanoid_armour_slot(**s).is_some())
            .count();
        assert_eq!(mapped, 4);
    }

    /// Every humanoid armour sheet 26.2 ships must actually decode out of the
    /// real jar at the path [`lodestone_assets::equipment`] computes, at the
    /// **64×32** the meshes' UVs assume.
    ///
    /// Ignored without a pack rather than skipped silently: an empty map is the
    /// fail-open production behaviour (armour just does not draw), which is
    /// exactly the state a path typo would also produce, so the only way to tell
    /// them apart is to assert against a real jar.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn every_humanoid_armour_sheet_decodes_from_the_real_jar() {
        use lodestone_assets::equipment::{ARMOUR_ASSETS, ArmourLayerType};

        let sheets = load_humanoid_armour_textures();
        assert!(
            !sheets.is_empty(),
            "no armour sheets loaded; set LODESTONE_ASSETS to a pack root with client.jar"
        );
        for asset in ARMOUR_ASSETS {
            for layer_type in [ArmourLayerType::Humanoid, ArmourLayerType::HumanoidLeggings] {
                for layer in asset.layers(layer_type) {
                    let img = sheets
                        .get(&(layer.texture, layer_type))
                        .unwrap_or_else(|| panic!("{}/{:?} did not load", layer.texture, layer_type));
                    assert_eq!(
                        (img.width, img.height),
                        (64, 32),
                        "{}/{:?} is not the 64x32 the armour meshes' UVs assume",
                        layer.texture,
                        layer_type
                    );
                }
            }
        }
        // Nine `humanoid` sheets (7 plain materials + leather's two layers,
        // where turtle_scute replaces leather's single-layer slot) and eight
        // `humanoid_leggings` ones (no turtle leggings exist).
        assert_eq!(sheets.len(), 17, "expected 9 humanoid + 8 leggings sheets");
    }

    /// Hermetic (no GPU): the whole armour resolution chain a live frame runs,
    /// from the `EntityDraw` the extract system produces through to the
    /// `(index range, wearer part)` pairs `prepare_armour` uploads.
    ///
    /// This is the *island* check for armour minus the pixels: it asserts that a
    /// zombie wearing a full diamond set produces attach points on a wearer
    /// resolved through the real corpus, and that each one indexes a real
    /// `part_transforms` entry with a positive determinant. What it cannot see —
    /// that `prepare_armour` is actually called and its batches drawn — is
    /// covered by `render_inner` calling it unconditionally next to
    /// `prepare_entities`.
    #[test]
    fn a_fully_armoured_zombie_resolves_layers_on_real_wearer_parts() {
        use lodestone_assets::ResourceLocation as Rl;
        use lodestone_assets::equipment::ArmourSlot;
        use lodestone_render::entity::{armour_layer_tint, armour_layers};

        let models = EntityModelSet::load();
        let armour = ArmourModelSet::load();
        let draw = EntityDraw {
            hurt: false,
            id: 7,
            type_path: "zombie".to_string(),
            item: None,
            equipment: vec![
                (
                    EquipmentSlot::Head,
                    Rl::parse("minecraft:diamond_helmet").unwrap(),
                ),
                (
                    EquipmentSlot::Chest,
                    Rl::parse("minecraft:leather_chestplate").unwrap(),
                ),
                (
                    EquipmentSlot::Legs,
                    Rl::parse("minecraft:iron_leggings").unwrap(),
                ),
                (
                    EquipmentSlot::Feet,
                    Rl::parse("minecraft:golden_boots").unwrap(),
                ),
                // Must be ignored: animal armour, not humanoid.
                (
                    EquipmentSlot::Body,
                    Rl::parse("minecraft:diamond_horse_armor").unwrap(),
                ),
            ],
            feet: Vec3::new(4.0, 70.0, -2.0),
            yaw: 41.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: AnimInput {
                head_yaw_deg: 5.0,
                head_pitch_deg: -3.0,
                limb_swing: 2.0,
                limb_swing_amount: 0.8,
                attack_anim: 0.0,
                age_ticks: 11.0,
                aggressive: false,
                ..AnimInput::REST
            },
            wool: None,
            count: 1,
            name_tag: None,
        };

        let instance = models
            .resolve(&draw.type_path, draw.feet, draw.yaw, draw.scale, &draw.anim)
            .expect("zombie resolves");
        let wearer = models.get(instance.model).expect("zombie mesh");

        let mut layer_count = 0;
        let mut attach_count = 0;
        for slot in ArmourSlot::ALL {
            let (_, id) = draw
                .equipment
                .iter()
                .find(|(s, _)| humanoid_armour_slot(*s) == Some(slot))
                .unwrap_or_else(|| panic!("{slot:?} equipped"));
            let layers = armour_layers(slot, id.path());
            assert!(!layers.is_empty(), "{slot:?} ({id}) resolved no layers");
            // Leather is the two-layer case; everything else is one.
            assert_eq!(
                layers.len(),
                if id.path().starts_with("leather") { 2 } else { 1 },
                "{slot:?} ({id}) layer count"
            );
            let mesh = armour.get(slot).expect("slot mesh");
            for (range, wearer_index) in mesh.attach(&wearer.skeleton) {
                let m = instance
                    .part_transforms
                    .get(wearer_index)
                    .expect("wearer part index is in range");
                assert!(range.index_count > 0);
                assert!(
                    m.determinant() > 0.0,
                    "{slot:?} armour rides a negative-determinant wearer matrix"
                );
                attach_count += 1;
            }
            layer_count += layers.len();
        }
        // 1 diamond helmet layer + 2 leather + 1 iron + 1 golden.
        assert_eq!(layer_count, 5);
        // head+hat, body+arms, body+legs, legs.
        assert_eq!(attach_count, 2 + 3 + 3 + 2);

        // `Body` contributed nothing: the horse armour must not have been read
        // as a chestplate.
        assert!(
            armour_layers(ArmourSlot::Chest, "diamond_horse_armor").is_empty(),
            "animal armour must not resolve as humanoid armour"
        );
        // And the leather tint is vanilla's undyed brown, in gamma bytes.
        let leather = armour_layers(ArmourSlot::Chest, "leather_chestplate");
        assert_eq!(
            armour_layer_tint(&leather[0]),
            lodestone_assets::equipment::UNDYED_LEATHER_RGB
        );
    }

    /// The sky reference must stay a plausible blue in the readback's own space;
    /// a value that drifted to the *displayed* colour would blow the "is this
    /// pixel sky?" test open, which is exactly how the two gates below broke.
    #[test]
    fn sky_reference_tracks_the_clear_colour() {
        assert_eq!(sky_clear_bytes(), [62, 118, 211]);
    }

    /// Hermetic (no GPU, no device): [`ThirdPersonBodyState::into_draw`] must
    /// hand back exactly the [`EntityDraw`] shape [`RenderState::render_inner`]
    /// folds into a frame's entity list, and that draw must actually resolve
    /// through the real model corpus — including the outer-layer overlay
    /// parts and a positive-determinant pose for every part — for *both*
    /// skin rigs. [`EntityModelSet::load`]/`resolve` are pure CPU (baking
    /// happens once at load, not per frame), so this needs no wgpu adapter.
    #[test]
    fn third_person_body_state_resolves_through_the_real_corpus() {
        let models = EntityModelSet::load();
        for slim in [false, true] {
            let state = ThirdPersonBodyState {
                feet: Vec3::new(1.0, 2.0, 3.0),
                body_yaw_deg: 123.0,
                anim: AnimInput {
                    head_yaw_deg: 10.0,
                    head_pitch_deg: -5.0,
                    limb_swing: 2.0,
                    limb_swing_amount: 1.0,
                    attack_anim: 0.0,
                    age_ticks: 15.0,
                    aggressive: false,
                    ..AnimInput::REST
                },
                scale: 1.0,
                slim,
                equipment: Vec::new(),
            };
            let expected_model = if slim { "player_slim" } else { "player_wide" };
            let draw = state.clone().into_draw();
            assert_eq!(draw.id, LOCAL_PLAYER_DRAW_ID);
            assert_eq!(draw.type_path, expected_model);
            assert_eq!(draw.feet, state.feet);
            assert_eq!(draw.yaw, state.body_yaw_deg);
            assert_eq!(draw.scale, state.scale);
            assert_eq!(draw.anim, state.anim);
            assert!(draw.item.is_none());
            assert!(draw.equipment.is_empty());

            let instance = models
                .resolve(&draw.type_path, draw.feet, draw.yaw, draw.scale, &draw.anim)
                .unwrap_or_else(|| panic!("{expected_model} must resolve through the corpus"));
            assert_eq!(instance.model, expected_model);
            let mesh = models.get(expected_model).expect("mesh");
            for overlay in [
                "hat",
                "jacket",
                "right_sleeve",
                "left_sleeve",
                "right_pants",
                "left_pants",
            ] {
                assert!(
                    mesh.skeleton.index_of(overlay).is_some(),
                    "{expected_model} is missing its outer-layer part {overlay:?} — an \
                     omitted overlay looks like a missing-skin-layer bug, not a missing \
                     feature"
                );
            }
            for (i, part) in instance.part_transforms.iter().enumerate() {
                assert!(
                    part.determinant() > 0.0,
                    "{expected_model} part {i}: determinant must be positive, was {}",
                    part.determinant()
                );
            }
        }
    }

    /// Headless GPU test: generate a world, mesh + upload every section, render
    /// one frame, and read pixels back to prove terrain (not just sky) drew.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn world_renders_terrain_with_pixel_readback() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let world = crate::worldgen::generate(2);
        let classifier = crate::blocks::DemoClassifier;
        let mut state = RenderState::new(device, queue, format, w, h, None);

        let mut total_quads = 0usize;
        let mut sections = 0usize;
        let radius = 2;
        for cz in -radius..=radius {
            for cx in -radius..=radius {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y: crate::worldgen::MIN_Y,
                    };
                    if let Some(snap) = crate::mesher::snapshot_section(&world, key) {
                        let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                        total_quads += mesh.quad_count();
                        sections += 1;
                        state.upload_section(
                            device,
                            queue,
                            key,
                            &crate::mesher::SectionGeometry::Packed(mesh),
                        );
                    }
                }
            }
        }
        assert!(sections > 0, "some sections should have meshed");

        // Camera above the origin, backed off to the north, looking south and
        // angled down over the terrain.
        let feet = crate::worldgen::spawn_feet();
        let camera = Camera {
            position: glam::Vec3::new(feet[0] as f32, feet[1] as f32 + 6.0, feet[2] as f32 - 18.0),
            yaw: 0.0,
            pitch: 22.0,
            fov_y_degrees: 70.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let start = std::time::Instant::now();
        let frame = target.acquire().expect("headless acquire");
        // Draw with a block outline enabled to exercise the outline pipeline.
        let stats = state.render(
            device,
            queue,
            frame.view(),
            &camera,
            Some([0, feet[1] as i32, 0]),
            &[],
        );
        let pixels = target.read_texels(device, queue);
        let frame_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Count pixels that clearly differ from the sky clear: terrain sprites
        // are green/brown/grey, far from sky blue.
        let sky = sky_clear_bytes();
        let mut terrain_px = 0usize;
        for px in pixels.chunks_exact(4) {
            let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                + (i32::from(px[1]) - i32::from(sky[1])).abs()
                + (i32::from(px[2]) - i32::from(sky[2])).abs();
            if d > 60 {
                terrain_px += 1;
            }
        }
        let coverage = terrain_px as f64 / (w * h) as f64;
        let sky_px = (w * h) as usize - terrain_px;
        let sky_coverage = sky_px as f64 / (w * h) as f64;

        eprintln!("=== shell world render (headless) ===");
        eprintln!("sections meshed   = {sections}");
        eprintln!("sections drawn    = {}", stats.sections_drawn);
        eprintln!("quads (meshed)    = {total_quads}");
        eprintln!("quads (drawn)     = {}", stats.total_quads);
        eprintln!("draw calls        = {}", stats.draw_calls);
        eprintln!("mesh VRAM (bytes) = {}", stats.vram_bytes);
        eprintln!("terrain coverage  = {:.1}%", coverage * 100.0);
        eprintln!("sky coverage      = {:.1}%", sky_coverage * 100.0);
        eprintln!("frame time (ms)   = {frame_ms:.3}");

        // Two-sided on purpose: a blank/all-sky frame fails the terrain guard,
        // and an all-terrain frame (camera stuck inside a block, full-screen
        // fog, a broken clear) fails the sky guard. "Correctly rendered nothing"
        // and "rendered one solid colour" must both be distinguishable from a
        // real horizon.
        assert!(
            coverage > 0.05,
            "expected visible terrain, only {:.1}% non-sky pixels",
            coverage * 100.0
        );
        assert!(
            sky_coverage > 0.05,
            "expected visible sky above the horizon, only {:.1}% sky pixels — \
             frame may be a solid fill rather than a rendered scene",
            sky_coverage * 100.0
        );
    }

    /// Headless proof that the block outline actually draws distinct pixels:
    /// render the same scene twice — once without an outline, once with one
    /// around a block squarely in view — and confirm the outline adds a modest
    /// number of near-black pixels where terrain used to be. Pixel readback is
    /// the project's evidence standard for "did it really render?".
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn block_outline_draws_visible_edges() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let world = crate::worldgen::generate(2);
        let classifier = crate::blocks::DemoClassifier;
        let mut state = RenderState::new(device, queue, format, w, h, None);
        for cz in -2..=2 {
            for cx in -2..=2 {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey {
                        cx,
                        cz,
                        si,
                        min_y: crate::worldgen::MIN_Y,
                    };
                    if let Some(snap) = crate::mesher::snapshot_section(&world, key) {
                        let mesh = crate::mesher::mesh_snapshot(&snap, &classifier);
                        state.upload_section(
                            device,
                            queue,
                            key,
                            &crate::mesher::SectionGeometry::Packed(mesh),
                        );
                    }
                }
            }
        }

        // Outline a cube floating in the air with open sky behind it, so its
        // edges are crisp black lines on blue and can't be confused with dark
        // terrain. The outline is a pure wireframe at world coords — it draws
        // whether or not a block occupies the cell.
        let target_block = [0i32, crate::worldgen::surface_height(0, 0) + 12, 6];
        let camera = Camera {
            position: glam::Vec3::new(0.5, target_block[1] as f32 + 0.5, -2.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let frame = target.acquire().expect("acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        let plain = target.read_texels(device, queue);

        let frame = target.acquire().expect("acquire");
        state.render(
            device,
            queue,
            frame.view(),
            &camera,
            Some(target_block),
            &[],
        );
        let outlined = target.read_texels(device, queue);

        // The only thing that changed between the two frames is the outline, so
        // count pixels whose colour moved. A blended 0.6-alpha black line darkens
        // whatever it covers; we detect the change directly rather than guessing
        // its final colour.
        let mut changed = 0usize;
        let mut darkened = 0usize;
        for (a, b) in plain.chunks_exact(4).zip(outlined.chunks_exact(4)) {
            let d = (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs();
            if d > 20 {
                changed += 1;
                // The outline can only darken (black over colour).
                if i32::from(b[0]) + i32::from(b[1]) + i32::from(b[2])
                    < i32::from(a[0]) + i32::from(a[1]) + i32::from(a[2])
                {
                    darkened += 1;
                }
            }
        }

        eprintln!("=== outline pixel readback ===");
        eprintln!("pixels changed by outline = {changed}");
        eprintln!("of which darkened         = {darkened}");

        assert!(
            changed > 50,
            "outline should visibly change the frame, only {changed} px moved"
        );
        assert_eq!(
            changed, darkened,
            "an outline only darkens pixels it covers"
        );
    }

    /// Headless proof that the debug-line pass — the render half of
    /// `ExtractSet::Debug` (`docs/plugin-api.md`) — actually draws pixels
    /// through [`RenderState::set_debug_lines_source`], not merely that a
    /// pipeline object exists. Same differential idiom as
    /// `block_outline_draws_visible_edges`: render the same scene with the
    /// source unset and with it returning a bright line across open sky, and
    /// confirm the second frame lit pixels the first did not.
    ///
    /// This is deliberately the *only* place that calls
    /// `set_debug_lines_source` in this repo today — see that method's docs,
    /// and [`DebugLinesSource`]'s, for why the ECS `DebugLines` resource is
    /// not actually polled by anything yet. This test proves the pipeline
    /// side works in isolation; it does not and cannot prove the ECS-to-here
    /// wire exists, because that wire is unbuilt.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn debug_lines_source_draws_visible_pixels() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        // Open sky, no terrain at all: nothing else in the scene could
        // account for a pixel changing between the two frames below.
        let mut state = RenderState::new(device, queue, format, w, h, None);

        let camera = Camera {
            position: glam::Vec3::new(0.5, 64.5, -2.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let frame = target.acquire().expect("acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        let without_lines = target.read_texels(device, queue);

        // A bright red line squarely in view, well inside the frustum near
        // and far planes, and thick enough (drawn as several parallel
        // segments) to survive the near-black outline's "only darkens" logic
        // not applying here — a bright line lightens sky-blue pixels.
        state.set_debug_lines_source(|| {
            let mut verts = Vec::new();
            for dy in [-0.5f32, 0.0, 0.5] {
                verts.push(DebugLineVertex {
                    position: [-3.0, 64.0 + dy, 4.0],
                    color: [1.0, 0.0, 0.0, 1.0],
                });
                verts.push(DebugLineVertex {
                    position: [3.0, 64.0 + dy, 4.0],
                    color: [1.0, 0.0, 0.0, 1.0],
                });
            }
            verts
        });

        let frame = target.acquire().expect("acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        let with_lines = target.read_texels(device, queue);

        let mut changed = 0usize;
        for (a, b) in without_lines
            .chunks_exact(4)
            .zip(with_lines.chunks_exact(4))
        {
            let d = (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs();
            if d > 20 {
                changed += 1;
            }
        }

        eprintln!("=== debug-line pixel readback ===");
        eprintln!("pixels changed by debug lines = {changed}");

        assert!(
            changed > 20,
            "installing a debug-lines source should visibly change the frame, \
             only {changed} px moved"
        );
    }

    /// Negative control for the test above: with no source installed (the
    /// default state of a fresh [`RenderState`]), two renders of the same
    /// scene must be pixel-identical. Without this, the assertion above could
    /// be satisfied by a pass that draws unconditionally regardless of
    /// whether a source was ever installed.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn no_debug_lines_source_installed_draws_nothing() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let state = RenderState::new(device, queue, format, w, h, None);
        let camera = Camera {
            position: glam::Vec3::new(0.5, 64.5, -2.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let frame = target.acquire().expect("acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        let first = target.read_texels(device, queue);

        let frame = target.acquire().expect("acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        let second = target.read_texels(device, queue);

        assert_eq!(
            first, second,
            "an unset debug-lines source must draw nothing"
        );
    }

    /// Headless proof that HUD **text actually rasterizes to pixels**, not just
    /// that geometry is generated. Renders two frames over the same known clear
    /// colour: an empty HUD (no crosshair/debug/chat) and one carrying chat
    /// lines plus a prompt. The empty frame must stay essentially background;
    /// the chat frame must light a substantial run of glyph pixels. Two-sided on
    /// purpose — a stray clear or wrong `LoadOp` lights the empty frame, and a
    /// no-op text path leaves the chat frame dark, so neither degenerate outcome
    /// can pass.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn hud_chat_text_rasterizes_to_pixels() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let clear = wgpu::Color {
            r: 0.04,
            g: 0.04,
            b: 0.08,
            a: 1.0,
        };
        let bg = [10i32, 10, 20];

        // Clear a fresh target to `clear`, render one HUD frame over it (the HUD
        // draws with `LoadOp::Load`), and count pixels far from the background.
        let lit_pixels = |frame: &crate::hud::HudFrame| -> usize {
            let mut target = HeadlessTarget::new(device, w, h, format);
            let mut hud = crate::hud::HudRenderer::new(device, format);
            let ht_frame = target.acquire().expect("headless acquire");
            {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("clear"),
                });
                {
                    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("hud-clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: ht_frame.view(),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                queue.submit(std::iter::once(enc.finish()));
            }
            hud.render(device, queue, ht_frame.view(), frame, w, h);
            let pixels = target.read_texels(device, queue);
            pixels
                .chunks_exact(4)
                .filter(|px| {
                    let d = (i32::from(px[0]) - bg[0]).abs()
                        + (i32::from(px[1]) - bg[1]).abs()
                        + (i32::from(px[2]) - bg[2]).abs();
                    d > 40
                })
                .count()
        };

        let stats = crate::hud::DebugStats::default();
        let empty_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            ..crate::hud::HudFrame::new(&stats)
        };
        let empty_lit = lit_pixels(&empty_frame);

        let chat = [("<Steve> hello world", 0.0_f32), ("<Alex> hi there", 0.0)];
        let chat_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            chat_input: Some("typing a message"),
            ..crate::hud::HudFrame::new(&stats)
        };
        let chat_lit = lit_pixels(&chat_frame);

        eprintln!("=== hud chat rasterization ===");
        eprintln!("empty HUD lit px = {empty_lit}");
        eprintln!("chat  HUD lit px = {chat_lit}");

        assert!(
            empty_lit < 20,
            "an empty HUD should read as background, but {empty_lit} px were lit — \
             a stray clear or wrong LoadOp is drawing something"
        );
        assert!(
            chat_lit > 200,
            "chat text should rasterize a substantial run of glyph pixels, only {chat_lit} lit — \
             the text path may be a no-op"
        );
    }

    /// The scoreboard sidebar must actually reach pixels. Same two-sided shape as
    /// the chat proof: an empty HUD stays background; a sidebar with two scored
    /// rows lights a substantial run of glyph pixels. A no-op fold, a panel drawn
    /// with no text, or a wrong `LoadOp` each fails one side.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn hud_sidebar_rasterizes_to_pixels() {
        use crate::overlay::{Sidebar, SidebarLine};
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let clear = wgpu::Color {
            r: 0.04,
            g: 0.04,
            b: 0.08,
            a: 1.0,
        };
        let bg = [10i32, 10, 20];

        let lit_pixels = |frame: &crate::hud::HudFrame| -> usize {
            let mut target = HeadlessTarget::new(device, w, h, format);
            let mut hud = crate::hud::HudRenderer::new(device, format);
            let ht_frame = target.acquire().expect("headless acquire");
            {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("clear"),
                });
                {
                    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("hud-clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: ht_frame.view(),
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                queue.submit(std::iter::once(enc.finish()));
            }
            hud.render(device, queue, ht_frame.view(), frame, w, h);
            let pixels = target.read_texels(device, queue);
            pixels
                .chunks_exact(4)
                .filter(|px| {
                    let d = (i32::from(px[0]) - bg[0]).abs()
                        + (i32::from(px[1]) - bg[1]).abs()
                        + (i32::from(px[2]) - bg[2]).abs();
                    d > 40
                })
                .count()
        };

        let stats = crate::hud::DebugStats::default();
        let empty_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            ..crate::hud::HudFrame::new(&stats)
        };
        let empty_lit = lit_pixels(&empty_frame);

        let side = Sidebar {
            title: "Objectives".into(),
            lines: vec![
                SidebarLine {
                    label: "Kills".into(),
                    score: "7".into(),
                },
                SidebarLine {
                    label: "Deaths".into(),
                    score: "2".into(),
                },
            ],
        };
        let side_frame = crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            sidebar: Some(&side),
            ..crate::hud::HudFrame::new(&stats)
        };
        let side_lit = lit_pixels(&side_frame);

        eprintln!("=== hud sidebar rasterization ===");
        eprintln!("empty   HUD lit px = {empty_lit}");
        eprintln!("sidebar HUD lit px = {side_lit}");

        assert!(
            empty_lit < 20,
            "an empty HUD should read as background, but {empty_lit} px were lit"
        );
        assert!(
            side_lit > 200,
            "the sidebar title, labels and scores should rasterize a substantial run \
             of glyph pixels, only {side_lit} lit — the fold or text path may be a no-op"
        );
    }

    /// Headless GPU test: render a single entity (no terrain) through the real
    /// [`RenderState::render`] path — the same call the live frame loop uses —
    /// and read pixels back to prove a mob reaches the screen. This is the
    /// shell-level analogue of `lodestone-render`'s `entity_gate`, but it drives
    /// the *shell's* wiring: `EntityDraw` → resolve → `plan_entities` → upload →
    /// instanced draw, sharing the terrain depth buffer.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn entity_renders_to_pixels_through_shell_path() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
             here would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let state = RenderState::new(device, queue, format, w, h, None);

        // A pig standing just in front of the camera, which looks south (+Z,
        // yaw 0) at eye level with the pig's body — mirrors the render-crate
        // gate's geometry so a regression there shows up here too.
        let pig_feet = glam::Vec3::new(0.0, 0.0, 4.0);
        let camera = Camera {
            position: glam::Vec3::new(0.0, 0.9, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 60.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };

        let draws = vec![
            EntityDraw {
                hurt: false,
                id: 1,
                type_path: "pig".to_owned(),
                item: None,
                feet: pig_feet,
                yaw: 0.0,
                head_yaw: 0.0,
                pitch: 0.0,
                scale: 1.0,
                anim: lodestone_render::AnimInput::REST,
                equipment: Vec::new(),
                wool: None,
                count: 1,
                name_tag: None,
            },
            // A second pig behind the camera so frustum culling has something
            // real to remove — the anti-vacuity guard on the cull path.
            EntityDraw {
                hurt: false,
                id: 2,
                type_path: "pig".to_owned(),
                item: None,
                feet: glam::Vec3::new(0.0, 0.0, -12.0),
                yaw: 0.0,
                head_yaw: 0.0,
                pitch: 0.0,
                scale: 1.0,
                anim: lodestone_render::AnimInput::REST,
                equipment: Vec::new(),
                wool: None,
                count: 1,
                name_tag: None,
            },
        ];

        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &draws);
        let pixels = target.read_texels(device, queue);

        assert_eq!(
            stats.entities_drawn, 1,
            "exactly the front pig should draw; the one behind the camera must cull \
             (drawn={}, culled={})",
            stats.entities_drawn, stats.entities_culled
        );
        assert!(
            stats.entities_culled >= 1,
            "the pig behind the camera should have been frustum-culled, but culled={}",
            stats.entities_culled
        );

        // The synthetic pig texture is a solid tint; count pixels that clearly
        // differ from the sky clear colour, and confirm they cluster in the
        // centre (where the pig is) rather than smeared across the frame.
        let sky = sky_clear_bytes();
        let is_mob = |px: &[u8]| -> bool {
            let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                + (i32::from(px[1]) - i32::from(sky[1])).abs()
                + (i32::from(px[2]) - i32::from(sky[2])).abs();
            d > 60
        };

        let mut mob_px = 0usize;
        let mut centre_px = 0usize;
        let mut corner_px = 0usize;
        let mut arm_px = 0usize;
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let x = (i as u32) % w;
            let y = (i as u32) / w;
            if is_mob(px) {
                mob_px += 1;
            }
            let cx = x >= w / 4 && x < 3 * w / 4;
            let cy = y >= h / 4 && y < 3 * h / 4;
            if cx && cy && is_mob(px) {
                centre_px += 1;
            }
            // The **bottom-right** corner is excluded on purpose: that is where
            // the unconditional first-person arm lives (`prepare_first_person_arm`
            // → `first_person_arm_pose`, camera-space, roughly the right-hand 30%
            // and bottom 30% of frame). This assertion is about the *pig* being
            // centred, and folding the arm into it would turn a working feature
            // into a red gate. The other three corners still have to stay sky, so
            // the "mob smeared across the whole frame" defect is still caught.
            let bottom_right = x >= w / 2 && y >= h / 2;
            let corner = (x < w / 8 || x >= 7 * w / 8) && (y < h / 8 || y >= 7 * h / 8);
            if corner && !bottom_right && is_mob(px) {
                corner_px += 1;
            }
            if bottom_right && is_mob(px) {
                arm_px += 1;
            }
        }
        let coverage = mob_px as f64 / (w * h) as f64;

        eprintln!("=== shell entity render (headless) ===");
        eprintln!("entities drawn  = {}", stats.entities_drawn);
        eprintln!("entities culled = {}", stats.entities_culled);
        eprintln!("mob coverage    = {:.2}%", coverage * 100.0);
        eprintln!("centre mob px   = {centre_px}");
        eprintln!("corner mob px   = {corner_px}");
        eprintln!("arm px (bot-rt) = {arm_px}");
        eprintln!("arm drawn       = {}", stats.first_person_arm_drawn);

        // Two-sided: the pig must reach pixels (not a blank frame) but not fill
        // the screen (a broken clear or a mob glued to the camera), and it must
        // be centred (the corners stay sky).
        assert!(
            mob_px > 200,
            "expected the pig to reach pixels, only {mob_px} non-sky px ({:.2}%)",
            coverage * 100.0
        );
        assert!(
            coverage < 0.6,
            "the pig should not fill the frame ({:.1}% non-sky) — a mob glued to the \
             near plane or a broken clear",
            coverage * 100.0
        );
        assert!(
            centre_px > 100,
            "the pig should sit in the centre of the frame, only {centre_px} centre px"
        );
        assert_eq!(
            corner_px, 0,
            "the frame corners should stay sky, but {corner_px} corner px read as mob"
        );

        // The first-person arm, on the same frame and for free: it is drawn
        // unconditionally in its own pass, so it must reach pixels in the
        // bottom-right quadrant. `first_person_arm_drawn` distinguishes "the pass
        // never ran" (a missing mesh/texture/part — a plumbing defect) from "it
        // ran and rasterised nothing" (a wrong pose or a winding flip), which look
        // identical from the pixel count alone.
        assert!(
            stats.first_person_arm_drawn,
            "the first-person arm pass must run: player_wide's mesh, texture and \
             arm part are all expected to exist"
        );
        assert!(
            arm_px > 500,
            "the first-person arm should fill a chunk of the bottom-right quadrant, \
             only {arm_px} non-sky px there — a wrong camera-space pose parks it at \
             the world origin, and an inverted winding culls every face"
        );
    }

    /// Headless GPU texture-correctness gate. The placeholder
    /// (`synthetic_entity_texture`) paints an entire mob a *single* flat hue
    /// (`model_tint`), varying only in brightness under lighting. A real per-mob
    /// sheet from `client.jar` carries several hues on one body — the zombie's
    /// green skin, teal shirt and dark-blue legs. So "a meaningful share of one
    /// mob's pixels sit at a hue far from any single flat tint" is a signal only
    /// the real sheet can produce. This renders the *same* zombie twice — once
    /// with the jar sheet, once forced back to the placeholder — and asserts the
    /// real render is markedly more multi-hued. If texture loading regresses to
    /// the fallback, the two renders converge and this reddens.
    ///
    /// This is the screen-capture-free stand-in for "look at the screenshot":
    /// screencapture needs Screen Recording permission the CI/agent host lacks,
    /// so instead of eyeballing the window we read the drawn pixels back and
    /// assert the mob's *colour* — not merely that something drew.
    #[test]
    #[ignore = "requires a GPU adapter and .cache/mc/26.2/client.jar"]
    fn zombie_wears_its_real_skin_not_the_flat_placeholder() {
        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter), don't 'skip' — a silent pass \
             here would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let mut target = HeadlessTarget::new(device, w, h, format);

        let mut state = RenderState::new(device, queue, format, w, h, None);

        // One zombie centred in front of a south-looking camera, framed on its
        // torso and head where the shirt/skin hues live.
        let camera = Camera {
            position: glam::Vec3::new(0.0, 1.4, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 60.0,
            aspect: w as f32 / h as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };
        let draws = vec![EntityDraw {
            hurt: false,
            id: 1,
            type_path: "zombie".to_owned(),
            item: None,
            feet: glam::Vec3::new(0.0, 0.0, 3.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            equipment: Vec::new(),
            wool: None,
            count: 1,
            name_tag: None,
        }];

        // Fraction of a mob's bright pixels whose *hue direction* is far from the
        // model's single flat placeholder tint. Brightness scaling (lighting)
        // leaves the direction unchanged, so under the placeholder this is ~0; a
        // real multi-hue sheet pushes it up.
        //
        // **Left half of the frame only.** The first-person arm is drawn
        // unconditionally into the bottom-right (see
        // `prepare_first_person_arm`), textured from `player_wide` — a *different*
        // model, so a different `model_tint` under the synthetic control. Its
        // pixels would land in `off` and blow the `off_syn < 0.05` control clean
        // open, making the gate red for a working feature. The zombie is centred
        // at `x = w/2` and vertically stratified (skin / shirt / legs), so its
        // left half carries every hue this gate is looking for, while the arm
        // starts around `x = 0.77·w` — a wide margin.
        let off_hue_fraction = |pixels: &[u8]| -> (usize, f64) {
            let sky = sky_clear_bytes().map(f32::from);
            let tint = model_tint("zombie");
            let tv = glam::Vec3::new(tint[0] as f32, tint[1] as f32, tint[2] as f32).normalize();
            let mut mob = 0usize;
            let mut off = 0usize;
            for (i, px) in pixels.chunks_exact(4).enumerate() {
                if (i as u32) % w >= w / 2 {
                    continue;
                }
                let c = glam::Vec3::new(px[0] as f32, px[1] as f32, px[2] as f32);
                let d = (c.x - sky[0]).abs() + (c.y - sky[1]).abs() + (c.z - sky[2]).abs();
                if d <= 60.0 {
                    continue; // sky
                }
                mob += 1;
                // Skip near-black shadow pixels where a hue direction is noise.
                if c.x + c.y + c.z < 60.0 {
                    continue;
                }
                let dir = c.normalize();
                if dir.dot(tv) < 0.95 {
                    off += 1;
                }
            }
            let frac = if mob == 0 {
                0.0
            } else {
                off as f64 / mob as f64
            };
            (mob, frac)
        };

        // Real jar sheet first.
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &draws);
        let real_px = target.read_texels(device, queue);
        let (mob_real, off_real) = off_hue_fraction(&real_px);
        assert_eq!(
            stats.entities_drawn, 1,
            "the zombie should draw exactly once (drawn={})",
            stats.entities_drawn
        );

        // Same mob, forced back to the flat placeholder — the built-in control.
        state.entities.force_synthetic_textures(device, queue);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &draws);
        let syn_px = target.read_texels(device, queue);
        let (mob_syn, off_syn) = off_hue_fraction(&syn_px);

        eprintln!("=== zombie texture-correctness gate ===");
        eprintln!("real: mob_px={mob_real} off_hue={:.1}%", off_real * 100.0);
        eprintln!("synth: mob_px={mob_syn} off_hue={:.1}%", off_syn * 100.0);

        assert!(
            mob_real > 300 && mob_syn > 300,
            "both renders must actually put the zombie on screen (real={mob_real}, \
             synth={mob_syn}) — otherwise the comparison is vacuous"
        );
        assert!(
            off_syn < 0.05,
            "the flat placeholder is a single hue, so its off-hue fraction must be \
             ~0, got {:.1}% — the control isn't controlling",
            off_syn * 100.0
        );
        assert!(
            off_real > 0.20,
            "the real zombie sheet should paint a substantial share of the body at \
             hues away from any single tint (green skin / teal shirt / dark legs), \
             got only {:.1}% — textures likely fell back to the placeholder",
            off_real * 100.0
        );
        assert!(
            off_real > off_syn * 4.0,
            "the real sheet must be markedly more multi-hued than the placeholder \
             (real {:.1}% vs synth {:.1}%) — if they're close, the real path is a \
             no-op and mobs are still flat",
            off_real * 100.0,
            off_syn * 100.0
        );
    }
}
