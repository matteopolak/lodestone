//! GPU resources and texture loading for the entity render pass: mobs,
//! humanoid armour layers, the sheep wool layer, and the mob-fire billboard.
use std::collections::{HashMap, HashSet};

use lodestone_assets::equipment::{ArmourLayerType, ArmourSlot};
use lodestone_render::entity_pipeline::flame_mesh;
use lodestone_render::{
    ArmourModelSet, CameraUniform, EntityCameraUniform, EntityModelSet, EntityPipeline,
    GpuEntityModel, SheepWoolModelSet, entity_camera_buffer, fog::FogUniform,
};

/// GPU resources for the entity pass: the instanced pipeline, one uploaded mesh
/// per model type, a per-model texture bind group, and a persistent camera
/// uniform rewritten each frame. Owns the version-free [`EntityModelSet`] so it
/// can resolve a live entity type into a renderable instance without the shell
/// naming a mob model directly.
///
/// Textures are the **real per-mob sheets** from `client.jar` when a vanilla
/// pack is present (loaded via [`crate::resources::load_entity_textures`]); a
/// model whose sheet is missing, or the offline demo world with no pack, falls
/// back to a synthetic solid colour so the mob stays visible and distinguishable
/// rather than invisible.
#[derive(Debug)]
pub(super) struct EntityRenderer {
    deferred_assets_pending: bool,
    deferred_assets_allowed: bool,
    deferred_stage: DeferredEntityStage,
    deferred_cursor: usize,
    deferred_manager: Option<lodestone_assets::ResourceManager>,
    deferred_trim_images:
        Option<Vec<(lodestone_assets::ResourceLocation, lodestone_assets::Image)>>,
    deferred_trim_decode_rx: Option<
        std::sync::mpsc::Receiver<
            Vec<(lodestone_assets::ResourceLocation, lodestone_assets::Image)>,
        >,
    >,
    deferred_variant_refs: Option<Vec<String>>,
    deferred_armour_specs: Option<Vec<((&'static str, ArmourLayerType), String)>>,
    pub(super) pipeline: EntityPipeline,
    texture_sampler: wgpu::Sampler,
    /// `PlayerModel`'s own `ENTITY_TRANSLUCENT` equivalent.  Player skins are
    /// the one ordinary-body texture family whose partially-alpha outer-layer
    /// texels must blend at the 26.2 `0.1` cutout threshold; mobs continue to
    /// use [`Self::pipeline`]'s opaque cutout contract.
    pub(super) player_skin_pipeline: wgpu::RenderPipeline,
    pub(super) models: EntityModelSet,
    pub(super) gpu_models: HashMap<&'static str, GpuEntityModel>,
    pub(super) textures: HashMap<&'static str, wgpu::BindGroup>,
    pub(super) cam_buffer: wgpu::Buffer,
    pub(super) cam_bind_group: wgpu::BindGroup,
    /// A **second** group-0 uniform, for the first-person arm pass.
    ///
    /// The arm is drawn in *camera space* with the projection alone — vanilla's
    /// `renderItemInHand` cancels the view matrix against the model-view stack
    /// (see [`hand_projection`]) — so its `view_proj` is a different matrix from
    /// the world one every frame, and the same buffer cannot serve both.
    ///
    /// This is a second bind *group* over the pipeline's **existing** group-0
    /// layout, not a fifth bind group: the entity shader still spends exactly
    /// two (camera + texture), and the model shader is untouched. Adding a fifth
    /// group anywhere would compile here on an M5 (8 groups) and crash at
    /// startup on any adapter at wgpu's 4-group floor.
    pub(super) hand_cam_buffer: wgpu::Buffer,
    pub(super) hand_cam_bind_group: wgpu::BindGroup,
    /// The humanoid-armour layers: a second pipeline (`LessEqual` depth — see
    /// [`EntityPipeline::armour_pipeline`]), the four slot meshes on the CPU
    /// (needed per frame to pair each part with the wearer's own part index) and
    /// on the GPU, and one texture bind group per `(texture name, layer type)`.
    ///
    /// `armour_textures` is **empty without a vanilla pack**, and armour then
    /// draws nothing rather than falling back to a synthetic colour the way a
    /// mob's own sheet does. That asymmetry is deliberate: a flat-magenta mob is
    /// recognisably "this mob's sheet is missing", whereas a flat-coloured
    /// helmet-shaped shell over a mob's head reads as a *rendering* bug, and the
    /// offline demo has no armour to draw in the first place.
    pub(super) armour_pipeline: wgpu::RenderPipeline,
    pub(super) armour_models: ArmourModelSet,
    pub(super) armour_gpu: Vec<(ArmourSlot, GpuEntityModel)>,
    pub(super) armour_textures: HashMap<(&'static str, ArmourLayerType), wgpu::BindGroup>,
    /// Smithing-table armour trims: one texture bind group per
    /// **trim sprite**, keyed by `lodestone_assets::trim::trim_sprite_id`'s
    /// output — `(pattern, material suffix, layer type)`, which is the granularity
    /// vanilla's palette swap actually produces.
    ///
    /// Drawn through [`Self::armour_pipeline`], **not**
    /// `EntityPipeline::trim_decal_pipeline`: that pipeline is the `decal: true`
    /// variant (depth `Equal`, no write) and all eighteen of 26.2's trim patterns
    /// are `decal: false`, so it stays selectable and unused. Reading the pipeline
    /// name as "the pipeline trims use" is the trap.
    ///
    /// Empty without a vanilla pack, and trims then draw nothing — the same
    /// deliberate asymmetry [`Self::armour_textures`] documents.
    pub(super) trim_textures: HashMap<lodestone_assets::ResourceLocation, wgpu::BindGroup>,
    /// The sheep wool layer: the one baked mesh on the CPU (needed
    /// per frame to pair each part with the wearer's own part index, the same
    /// as `armour_models`), its GPU upload, and its one texture bind group.
    ///
    /// Drawn through the **base** entity pipeline (`self.pipeline`) rather than
    /// `armour_pipeline` — wool has no second layer at the same inflation as
    /// itself, so there is no coplanar z-fighting to correct for the way
    /// leather's dyeable base and overlay need. Since that fix the two
    /// pipelines are depth-identical anyway (both `LessEqual`, vanilla's
    /// `ENTITY_SNIPPET` value), so this choice is now about which pass the draw
    /// belongs to, not about which depth compare it gets. See
    /// `docs/entity-rendering.md`.
    ///
    /// `wool_texture` is `None` without a vanilla pack, and wool then draws
    /// nothing rather than falling back to a synthetic colour — the same
    /// asymmetry `armour_textures` documents, for the same reason: a
    /// flat-coloured fleece shell reads as a rendering bug.
    pub(super) wool_models: SheepWoolModelSet,
    pub(super) wool_gpu: Option<GpuEntityModel>,
    pub(super) wool_texture: Option<wgpu::BindGroup>,
    /// The player cape overlay: one baked mesh (code-defined geometry, no
    /// pack dependency — see [`lodestone_assets::entity::player_cape_model`]),
    /// uploaded once, unconditionally. Unlike [`Self::wool_texture`] there is
    /// no single fixed texture here: a cape's sheet is a **per-player URL**,
    /// so the draw looks its texture up in [`Self::player_skins`] by that
    /// player's cape URL — the exact same fetch/cache pipeline a skin uses,
    /// reused rather than duplicated (see `remote_skins::RemoteSkin::cape`'s
    /// doc). `cape_gpu` is `None` only if the bake produced no geometry,
    /// which would be a code bug, not a missing pack.
    pub(super) cape_model: lodestone_render::CapeMesh,
    pub(super) cape_gpu: Option<GpuEntityModel>,
    /// The elytra wings layer: one baked mesh (code-defined geometry, no pack
    /// dependency — see [`lodestone_assets::entity::elytra_model`]), uploaded
    /// once, plus **one** jar texture bind group, unlike the cape's per-player
    /// URL. `elytra_model.parts` is needed on the CPU each frame to pair each
    /// wing with the wearer's own `"body"` part index, exactly as
    /// `armour_models`/`wool_models` are.
    ///
    /// A player's own sheet can override the built-in one: the production
    /// preference is the installed custom elytra URL, then an installed visible
    /// cape URL, and finally `ELYTRA_TEXTURE_PATH`. Profile parsing retains both
    /// custom URLs on `crate::remote_skins::RemoteSkin`; the per-frame pass
    /// selects the first installed identity and falls back to this bind group.
    ///
    /// `elytra_texture` is `None` without a vanilla pack, and the wings then
    /// draw nothing for anyone without a cape — the same asymmetry
    /// [`Self::wool_texture`] documents, for the same reason.
    pub(super) elytra_model: lodestone_render::ElytraMesh,
    pub(super) elytra_gpu: Option<GpuEntityModel>,
    pub(super) elytra_texture: Option<wgpu::BindGroup>,
    /// Paintings: one baked mesh per **shape**, one texture bind group per
    /// **variant**, plus the shared back/edge tile.
    ///
    /// # Why the mesh is keyed by shape and the texture by variant
    ///
    /// A painting's geometry is a function of `(width, height)` alone — nine
    /// distinct shapes across 26.2's 51 variants — while its front texture is
    /// per variant. Baking per shape rather than per variant is therefore 9
    /// vertex buffers instead of 51 for identical pixels. Each model carries
    /// **two** parts, in this order: `parts[0]` is the front face (sampling the
    /// variant's own sprite) and `parts[1]` the back and four edges (sampling
    /// `back.png`), because this engine binds one texture per draw and vanilla
    /// escapes that only by stitching both into its paintings atlas.
    ///
    /// `painting_textures` is keyed by
    /// [`lodestone_render::painting::PAINTING_VARIANTS`]' own `&'static str`,
    /// which is what `EntityDraw::painting` has already been narrowed to — so a
    /// miss here means the jar had no such sprite, not that the wire said
    /// something unexpected.
    ///
    /// All three are empty/`None` without a vanilla pack, and paintings then
    /// draw nothing — the same asymmetry [`Self::wool_texture`] documents. A
    /// flat-coloured rectangle in place of a painting would read as a rendering
    /// bug.
    pub(super) painting_models: Vec<(lodestone_render::painting::PaintingSize, GpuEntityModel)>,
    pub(super) painting_textures: HashMap<&'static str, wgpu::BindGroup>,
    pub(super) painting_back_texture: Option<wgpu::BindGroup>,
    /// The mob-fire billboard (player report: "mobs dont show
    /// flames yet"): a fourth pipeline
    /// ([`EntityPipeline::flame_pipeline`]) drawn through the **base**
    /// pipeline's own camera bind group (flame needs no camera data the mob
    /// pass does not already have), one baked mesh per entity type keyed by
    /// its network type path (built eagerly for every
    /// `lodestone_data::entity_types` name with a known
    /// `lodestone_data::entity_dimensions` entry — see [`Self::new`]), and one
    /// texture bind group for the combined `fire_0`/`fire_1` strip.
    ///
    /// `flame_texture` is `None` without a vanilla pack, and fire then draws
    /// nothing — the same asymmetry [`Self::armour_textures`]/
    /// [`Self::wool_texture`] document: a synthetic placeholder flame would
    /// read as a rendering bug, not as "no pack found".
    pub(super) flame_pipeline: wgpu::RenderPipeline,
    pub(super) flame_gpu_models: HashMap<String, GpuEntityModel>,
    pub(super) flame_texture: Option<wgpu::BindGroup>,
    /// The experience-orb billboard: a fifth pipeline
    /// ([`EntityPipeline::orb_pipeline`], alpha-blended with a `0.1` cutout)
    /// drawn through the **base** pipeline's own camera bind group, one mesh
    /// holding all eleven sprite cells as eleven *parts*, and one texture bind
    /// group for `entity/experience/experience_orb.png`.
    ///
    /// # Eleven parts of one mesh, not eleven meshes
    ///
    /// Vanilla's own experience-orb icon lookup buckets an orb's value into one of eleven
    /// 16×16 cells, and the cell is baked into the quad's **UVs** — so the
    /// geometry differs per cell and cannot be one instanced quad. Making them
    /// eleven `PartRange`s of a single 44-vertex buffer means the whole orb pass
    /// is one vertex/index binding and one instance buffer per cell actually on
    /// screen, which is the same shape the block-entity pass already draws
    /// per-part instances with. The part **index is the icon index**; see
    /// `RenderState::prepare_orbs`.
    ///
    /// `orb_texture` is `None` without a vanilla pack, and orbs then draw
    /// nothing — the same asymmetry [`Self::flame_texture`]/[`Self::wool_texture`]
    /// document. A synthetic green square would read as a rendering bug.
    pub(super) orb_pipeline: wgpu::RenderPipeline,
    pub(super) orb_gpu_model: Option<GpuEntityModel>,
    pub(super) orb_texture: Option<wgpu::BindGroup>,
    /// The camera-facing entity sprites
    /// ([`lodestone_render::entity_sprite::ENTITY_SPRITES`]) — one shared mesh
    /// with one [`lodestone_render::PartRange`] per sprite, plus one texture
    /// bind group per sprite.
    ///
    /// # Same shape as the orb above, one bind group wider
    ///
    /// The orb's eleven cells all live on **one** sheet, so it needs a single
    /// bind group and selects a cell by part index. These two sprites are two
    /// separate standalone sheets, so the part index selects the geometry and
    /// the parallel `sprite_textures` entry selects the sheet; the pass rebinds
    /// group 1 per batch. Two sprites, so the rebind is at most one extra
    /// binding per frame.
    ///
    /// They ride the **base** entity pipeline rather than a sixth of their own:
    /// both vanilla renderers use `RenderTypes.entityCutout`/`entityCutoutCull`,
    /// which is `DepthStencilState.DEFAULT` plus a `0.5` alpha cutout — exactly
    /// what `build_entity_pipeline`'s `fs_main` arm already is. The orb needed
    /// its own pipeline because `ENTITY_TRANSLUCENT` blends and cuts at `0.1`;
    /// nothing here does.
    ///
    /// An entry is `None` when the vanilla pack has no such sheet, and that
    /// sprite then draws nothing — the same asymmetry
    /// [`Self::orb_texture`]/[`Self::flame_texture`] document, and for the same
    /// reason: a magenta stand-in reads as a rendering bug.
    pub(super) sprite_gpu_model: Option<GpuEntityModel>,
    /// One entry per row of [`lodestone_render::entity_sprite::ENTITY_SPRITES`],
    /// in that table's order, so an index is valid for both this and
    /// `sprite_gpu_model`'s part list. See [`Self::sprite_gpu_model`].
    pub(super) sprite_textures: Vec<Option<wgpu::BindGroup>>,
    /// The boat water-clip mask (owner report: "placing down a boat still
    /// shows water through the bottom"): a sixth pipeline
    /// ([`EntityPipeline::water_mask_pipeline`], colour writes disabled)
    /// drawn through the **base** pipeline's own camera bind group. Unlike
    /// the flame/orb pipelines above, this needs **no** dedicated geometry or
    /// texture storage here: `"boat_water_patch"` is an ordinary
    /// [`lodestone_assets::entity_models`] corpus entry, so [`Self::models`]/
    /// [`Self::gpu_models`]/[`Self::textures`] already carry it through the
    /// same loop every other rig goes through — the pipeline object is the
    /// only thing this pass needs that a normal batch does not already have.
    /// See `gpu/entity_passes.rs`'s `prepare_entities` for where the second,
    /// per-boat instance is built into the dedicated water-mask phase, and
    /// `gpu/frame.rs` for the draw immediately before translucent water.
    pub(super) water_mask_pipeline: wgpu::RenderPipeline,
    /// The entity ground-shadow decal (owner report: "entity shadows are
    /// missing"): a seventh pipeline ([`EntityPipeline::shadow_pipeline`]),
    /// one texture bind group for `textures/misc/shadow.png`, and — unlike
    /// every sibling pipeline above — **no** stored geometry at all, because
    /// every shadow piece is unique, plain (non-instanced) vertex data built
    /// fresh each frame by `RenderState::prepare_shadows`.
    ///
    /// `shadow_texture` is `None` without a vanilla pack, and shadows then
    /// draw nothing — the same asymmetry [`Self::flame_texture`]/
    /// [`Self::orb_texture`] document.
    pub(super) shadow_pipeline: wgpu::RenderPipeline,
    pub(super) shadow_texture: Option<wgpu::BindGroup>,
    /// Remote players' fetched skins: one texture bind group per **texture
    /// URL**, filled in at runtime by
    /// [`RenderState::install_pending_player_skins`](super::RenderState::install_pending_player_skins).
    ///
    /// The only map here that grows *after* startup, and the reason it is keyed by
    /// `String` while [`Self::textures`] is keyed by `&'static str`: a fetched
    /// skin's identity arrives on the wire, so it cannot be a static name without
    /// leaking one string per distinct skin per session. A URL rather than a
    /// player UUID so two accounts wearing the same skin share one bind group,
    /// and so the key survives a reconnect.
    ///
    /// A miss is **not** a failure: the draw falls back to the model's own sheet
    /// from [`Self::textures`], so a remote player is Steve while their skin is in
    /// flight and themselves afterwards. Empty against every offline-mode server,
    /// which sends no `textures` property at all — see `crate::remote_skins`.
    pub(super) player_skins: HashMap<String, wgpu::BindGroup>,
    /// Last retained-skin generation incorporated into `player_skins`. This
    /// keeps cache recovery to one atomic load per frame and avoids cloning the
    /// retained URL map in the steady state.
    player_skins_epoch: u64,
    /// Variant mob sheets — one bind group per corpus **reference**
    /// (`entity/wolf/wolf_ashen`), from
    /// [`crate::resources::load_entity_variant_textures`].
    ///
    /// This is what gives `EntityTexture::resolve` a production reader. Before it,
    /// the corpus modelled nine wolf breeds and three climate skins and the whole
    /// render path asked only for `default_path()`, so every wolf drew pale and
    /// every pig drew temperate.
    ///
    /// Keyed by `String` rather than `&'static str` for the same reason
    /// [`Self::player_skins`] is: the key is derived from a jar listing at load
    /// time, so making it static would mean leaking one string per sheet.
    ///
    /// A miss is **not** a failure — the draw falls back to
    /// [`Self::textures`]' per-model sheet, which is exactly the previous
    /// behaviour. Empty with no vanilla pack.
    pub(super) variant_textures: HashMap<String, wgpu::BindGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredEntityStage {
    Models,
    Variants,
    ArmourModels,
    ArmourTextures,
    Trims,
    Wool,
    Cape,
    Elytra,
    PaintingsModels,
    PaintingsTextures,
    FlameModels,
    FlameTexture,
    Orb,
    SpritesModel,
    SpritesTextures,
    Shadow,
    Complete,
}

const DEFERRED_WORK_ITEMS_PER_FRAME: usize = 4;
#[cfg(any(target_arch = "wasm32", test))]
const TRIM_PERMUTATIONS_PER_BATCH: usize = 4;

#[derive(Debug)]
struct DeferredWorkBudget {
    remaining: usize,
    consumed: usize,
}

impl DeferredWorkBudget {
    fn new() -> Self {
        Self {
            remaining: DEFERRED_WORK_ITEMS_PER_FRAME,
            consumed: 0,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        self.consumed += 1;
        true
    }
}

impl EntityRenderer {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        let started = crate::platform::Instant::now();
        let pipeline = EntityPipeline::new(device, color_format);
        let player_skin_pipeline = pipeline.player_skin_pipeline(device, color_format);
        let cpu_started = crate::platform::Instant::now();
        let models = EntityModelSet::load();
        let models_ms = cpu_started.elapsed().as_secs_f64() * 1000.0;
        let cpu_started = crate::platform::Instant::now();
        let armour_models = ArmourModelSet::load();
        let armour_ms = cpu_started.elapsed().as_secs_f64() * 1000.0;
        let cpu_started = crate::platform::Instant::now();
        let wool_models = SheepWoolModelSet::load();
        let wool_ms = cpu_started.elapsed().as_secs_f64() * 1000.0;
        let cpu_started = crate::platform::Instant::now();
        let cape_model = lodestone_render::CapeMesh::load();
        let cape_ms = cpu_started.elapsed().as_secs_f64() * 1000.0;
        let cpu_started = crate::platform::Instant::now();
        let elytra_model = lodestone_render::ElytraMesh::load();
        let elytra_ms = cpu_started.elapsed().as_secs_f64() * 1000.0;
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-entity-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let mut gpu_models = HashMap::new();
        let mut textures = HashMap::new();
        for (name, mesh) in models.iter().filter(|(name, _)| {
            matches!(*name, "player_wide" | "player_slim")
        }) {
            if let Some(gpu) = GpuEntityModel::upload(device, mesh) {
                gpu_models.insert(name, gpu);
            }
            let view = synthetic_entity_texture(device, queue, name).0;
            textures.insert(name, pipeline.texture_bind_group(device, &view, &sampler));
        }

        let armour_pipeline = pipeline.armour_pipeline(device, color_format);
        let flame_pipeline = pipeline.flame_pipeline(device, color_format);
        let orb_pipeline = pipeline.orb_pipeline(device, color_format);
        let water_mask_pipeline = pipeline.water_mask_pipeline(device, color_format);
        let shadow_pipeline = pipeline.shadow_pipeline(device, color_format);
        let cam_buffer = entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform {
                    view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::disabled(),
            },
        );
        let cam_bind_group = pipeline.camera_bind_group(device, &cam_buffer);
        let hand_cam_buffer = entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform {
                    view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::disabled(),
            },
        );
        let hand_cam_bind_group = pipeline.camera_bind_group(device, &hand_cam_buffer);

        let renderer = Self {
            deferred_assets_pending: true,
            deferred_assets_allowed: false,
            deferred_stage: DeferredEntityStage::Models,
            deferred_cursor: 0,
            deferred_manager: None,
            deferred_trim_images: None,
            deferred_trim_decode_rx: None,
            deferred_variant_refs: None,
            deferred_armour_specs: None,
            pipeline,
            texture_sampler: sampler,
            player_skin_pipeline,
            models,
            gpu_models,
            textures,
            cam_buffer,
            cam_bind_group,
            hand_cam_buffer,
            hand_cam_bind_group,
            armour_pipeline,
            armour_models,
            armour_gpu: Vec::new(),
            armour_textures: HashMap::new(),
            trim_textures: HashMap::new(),
            wool_models,
            wool_gpu: None,
            wool_texture: None,
            cape_model,
            cape_gpu: None,
            elytra_model,
            elytra_gpu: None,
            elytra_texture: None,
            painting_models: Vec::new(),
            painting_textures: HashMap::new(),
            painting_back_texture: None,
            flame_pipeline,
            flame_gpu_models: HashMap::new(),
            flame_texture: None,
            orb_pipeline,
            orb_gpu_model: None,
            orb_texture: None,
            sprite_gpu_model: None,
            sprite_textures: Vec::new(),
            water_mask_pipeline,
            shadow_pipeline,
            shadow_texture: None,
            player_skins: HashMap::new(),
            player_skins_epoch: 0,
            variant_textures: HashMap::new(),
        };
        tracing::info!(
            target: "startup_profile",
            phase = "entity_renderer_core",
            models_ms,
            armour_ms,
            wool_ms,
            cape_ms,
            elytra_ms,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "core entity resources ready"
        );
        renderer
    }

    pub(super) fn initialize_deferred_assets(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) {
        if !self.deferred_assets_pending || !self.deferred_assets_allowed {
            return;
        }
        let started = crate::platform::Instant::now();
        let stage_before = self.deferred_stage;
        if self.deferred_manager.is_none() {
            self.deferred_manager = crate::resources::vanilla_manager();
        }
        let mut budget = DeferredWorkBudget::new();
        while self.deferred_assets_pending
            && self.deferred_step(device, queue, color_format, &mut budget)
        {}
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        if budget.consumed > 0 || stage_before != self.deferred_stage {
            tracing::info!(
                target: "startup_profile",
                phase = "entity_renderer_deferred",
                stage = ?stage_before,
                work_items = budget.consumed,
                elapsed_ms,
                pending = self.deferred_assets_pending,
                "deferred entity resource batch"
            );
        } else {
            tracing::trace!(
                target: "startup_profile",
                phase = "entity_renderer_deferred_wait",
                stage = ?stage_before,
                elapsed_ms,
                "deferred entity resources are waiting for an async dependency"
            );
        }
    }

    pub(super) fn allow_deferred_assets(&mut self) {
        self.deferred_assets_allowed = true;
    }

    #[cfg(test)]
    pub(super) fn deferred_assets_pending(&self) -> bool {
        self.deferred_assets_pending
    }

    fn deferred_step(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _color_format: wgpu::TextureFormat,
        budget: &mut DeferredWorkBudget,
    ) -> bool {
        loop {
            match self.deferred_stage {
                DeferredEntityStage::Models => {
                    let Some((name, mesh)) = self.models.iter().nth(self.deferred_cursor) else {
                        self.deferred_stage = DeferredEntityStage::Variants;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    let name = name;
                    if !self.gpu_models.contains_key(name) {
                        if let Some(gpu) = GpuEntityModel::upload(device, mesh) {
                            self.gpu_models.insert(name, gpu);
                        }
                    }
                    let view = self
                        .deferred_manager
                        .as_ref()
                        .and_then(|manager| load_entity_image(manager, name))
                        .map(|image| entity_texture_from_image(device, queue, &image))
                        .unwrap_or_else(|| synthetic_entity_texture(device, queue, name).0);
                    let bg = self.pipeline.texture_bind_group(device, &view, &self.texture_sampler);
                    self.textures.insert(name, bg);
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::Variants => {
                    if self.deferred_variant_refs.is_none() {
                        self.deferred_variant_refs = Some(
                            self.deferred_manager
                                .as_ref()
                                .map(variant_references)
                                .unwrap_or_default(),
                        );
                    }
                    let refs = self
                        .deferred_variant_refs
                        .as_ref()
                        .expect("variant refs set");
                    let Some(reference) = refs.get(self.deferred_cursor) else {
                        self.deferred_stage = DeferredEntityStage::ArmourModels;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    let reference = reference.clone();
                    if let Some(image) = self
                        .deferred_manager
                        .as_ref()
                        .and_then(|manager| load_reference_image(manager, &reference))
                    {
                        let view = entity_texture_from_image(device, queue, &image);
                        let bg = self.pipeline.texture_bind_group(device, &view, &self.texture_sampler);
                        self.variant_textures.insert(reference, bg);
                    }
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::ArmourModels => {
                    let Some(&slot) = ArmourSlot::ALL.get(self.deferred_cursor) else {
                        self.deferred_stage = DeferredEntityStage::ArmourTextures;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    if let Some(mesh) = self.armour_models.get(slot)
                        && let Some(gpu) = GpuEntityModel::upload_armour(device, mesh)
                    {
                        self.armour_gpu.push((slot, gpu));
                    }
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::ArmourTextures => {
                    if self.deferred_armour_specs.is_none() {
                        self.deferred_armour_specs = Some(armour_texture_specs());
                    }
                    let specs = self
                        .deferred_armour_specs
                        .as_ref()
                        .expect("armour specs set");
                    let Some((key, path)) = specs.get(self.deferred_cursor) else {
                        self.deferred_stage = DeferredEntityStage::Trims;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    let key = *key;
                    if let Some(image) = self
                        .deferred_manager
                        .as_ref()
                        .and_then(|manager| load_image(manager, path, "humanoid armour"))
                    {
                        let view = entity_texture_from_image(device, queue, &image);
                        let bg = self.pipeline.texture_bind_group(device, &view, &self.texture_sampler);
                        self.armour_textures.insert(key, bg);
                    }
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::Trims => {
                    if self.deferred_trim_decode_rx.is_none() && self.deferred_trim_images.is_none() {
                        if !budget.take() {
                            return false;
                        }
                        self.deferred_trim_decode_rx = Some(start_trim_decode());
                        return true;
                    }
                    if self.deferred_trim_images.is_none() {
                        let Some(receiver) = self.deferred_trim_decode_rx.as_ref() else {
                            return false;
                        };
                        match receiver.try_recv() {
                            Ok(mut images) => {
                                images.sort_by(|(left, _), (right, _)| left.cmp(right));
                                tracing::info!(
                                    target: "startup_profile",
                                    phase = "entity_renderer_trim_decode",
                                    count = images.len(),
                                    "deferred trim images ready"
                                );
                                self.deferred_trim_images = Some(images);
                                self.deferred_trim_decode_rx = None;
                                self.deferred_cursor = 0;
                                continue;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                self.deferred_trim_images = Some(Vec::new());
                                self.deferred_trim_decode_rx = None;
                                self.deferred_cursor = 0;
                                continue;
                            }
                        }
                    }
                    let images = self.deferred_trim_images.as_ref().expect("trim images set");
                    let Some((id, image)) = images.get(self.deferred_cursor) else {
                        self.deferred_stage = DeferredEntityStage::Wool;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    let id = id.clone();
                    let view = entity_texture_from_image(device, queue, image);
                    let bg = self
                        .pipeline
                        .texture_bind_group(device, &view, &self.texture_sampler);
                    self.trim_textures.insert(id, bg);
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::Wool => {
                    if self.deferred_cursor == 0 {
                        if !budget.take() {
                            return false;
                        }
                        self.wool_gpu = GpuEntityModel::upload_wool(device, self.wool_models.mesh());
                        self.deferred_cursor = 1;
                        return true;
                    }
                    if self.deferred_cursor == 1 {
                        if !budget.take() {
                            return false;
                        }
                        self.wool_texture = self
                            .deferred_manager
                            .as_ref()
                            .and_then(load_sheep_wool_texture_from_manager)
                            .map(|image| {
                                let view = entity_texture_from_image(device, queue, &image);
                                self.pipeline.texture_bind_group(device, &view, &self.texture_sampler)
                            });
                        self.deferred_stage = DeferredEntityStage::Cape;
                        self.deferred_cursor = 0;
                        return true;
                    }
                }
                DeferredEntityStage::Cape => {
                    if !budget.take() {
                        return false;
                    }
                    self.cape_gpu = GpuEntityModel::upload_cape(device, &self.cape_model);
                    self.deferred_stage = DeferredEntityStage::Elytra;
                    self.deferred_cursor = 0;
                    return true;
                }
                DeferredEntityStage::Elytra => {
                    if self.deferred_cursor == 0 {
                        if !budget.take() {
                            return false;
                        }
                        self.elytra_gpu = GpuEntityModel::upload_parts(
                            device,
                            &self.elytra_model.vertices,
                            &self.elytra_model.indices,
                            self.elytra_model.parts.iter().map(|(_, range)| *range).collect(),
                        );
                        self.deferred_cursor = 1;
                        return true;
                    }
                    if self.deferred_cursor == 1 {
                        if !budget.take() {
                            return false;
                        }
                        self.elytra_texture = self
                            .deferred_manager
                            .as_ref()
                            .and_then(load_elytra_texture_from_manager)
                            .map(|image| {
                                let view = entity_texture_from_image(device, queue, &image);
                                self.pipeline.texture_bind_group(device, &view, &self.texture_sampler)
                            });
                        self.deferred_stage = DeferredEntityStage::PaintingsModels;
                        self.deferred_cursor = 0;
                        return true;
                    }
                }
                DeferredEntityStage::PaintingsModels => {
                    let sizes = lodestone_render::painting::painting_sizes();
                    let Some(&size) = sizes.get(self.deferred_cursor) else {
                        self.deferred_stage = DeferredEntityStage::PaintingsTextures;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    if let Some(gpu) = upload_painting_model(device, size) {
                        self.painting_models.push((size, gpu));
                    }
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::PaintingsTextures => {
                    let variants = lodestone_render::painting::PAINTING_VARIANTS;
                    if self.deferred_cursor < variants.len() {
                        if !budget.take() {
                            return false;
                        }
                        let name = variants[self.deferred_cursor].0;
                        if let Some(image) = self
                            .deferred_manager
                            .as_ref()
                            .and_then(|manager| load_image(manager, &lodestone_render::painting::painting_texture_path(name), "painting sprite"))
                        {
                            let view = entity_texture_from_image(device, queue, &image);
                    let bg = self.pipeline.texture_bind_group(device, &view, &self.texture_sampler);
                            self.painting_textures.insert(name, bg);
                        }
                        self.deferred_cursor += 1;
                        return true;
                    }
                    if self.deferred_cursor == variants.len() {
                        if !budget.take() {
                            return false;
                        }
                        self.painting_back_texture = self
                            .deferred_manager
                            .as_ref()
                            .and_then(|manager| load_image(manager, lodestone_render::painting::PAINTING_BACK_TEXTURE, "painting back tile"))
                            .map(|image| {
                                let view = entity_texture_from_image(device, queue, &image);
                                self.pipeline.texture_bind_group(device, &view, &self.texture_sampler)
                            });
                        self.deferred_stage = DeferredEntityStage::FlameModels;
                        self.deferred_cursor = 0;
                        return true;
                    }
                }
                DeferredEntityStage::FlameModels => {
                    let Some(entity_type) = lodestone_data::entity_type::EntityType::all()
                        .nth(self.deferred_cursor)
                    else {
                        self.deferred_stage = DeferredEntityStage::FlameTexture;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    let path = entity_type.path();
                    let dims = lodestone_data::entity_dimensions::base_dimensions(entity_type);
                    let (vertices, indices) = flame_mesh(dims.width, dims.height);
                    if let Some(gpu) = GpuEntityModel::upload_parts(
                        device,
                        &vertices,
                        &indices,
                        vec![lodestone_render::PartRange {
                            index_start: 0,
                            index_count: indices.len() as u32,
                            vertex_start: 0,
                            vertex_count: vertices.len() as u32,
                        }],
                    ) {
                        self.flame_gpu_models.insert(path.to_owned(), gpu);
                    }
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::FlameTexture => {
                    if !budget.take() {
                        return false;
                    }
                    self.flame_texture = self
                        .deferred_manager
                        .as_ref()
                        .and_then(load_flame_textures_from_manager)
                        .map(|image| {
                            let view = entity_texture_from_image(device, queue, &image);
                            self.pipeline.texture_bind_group(device, &view, &self.texture_sampler)
                        });
                    self.deferred_stage = DeferredEntityStage::Orb;
                    self.deferred_cursor = 0;
                    return true;
                }
                DeferredEntityStage::Orb => {
                    if self.deferred_cursor == 0 {
                        if !budget.take() {
                            return false;
                        }
                        self.orb_gpu_model = upload_orb_model(device);
                        self.deferred_cursor = 1;
                        return true;
                    }
                    if self.deferred_cursor == 1 {
                        if !budget.take() {
                            return false;
                        }
                        self.orb_texture = self
                            .deferred_manager
                            .as_ref()
                            .and_then(load_orb_texture_from_manager)
                            .map(|image| {
                                let view = entity_texture_from_image(device, queue, &image);
                                self.pipeline.texture_bind_group(device, &view, &self.texture_sampler)
                            });
                        self.deferred_stage = DeferredEntityStage::SpritesModel;
                        self.deferred_cursor = 0;
                        return true;
                    }
                }
                DeferredEntityStage::SpritesModel => {
                    if !budget.take() {
                        return false;
                    }
                    self.sprite_gpu_model = upload_sprite_model(device);
                    self.deferred_stage = DeferredEntityStage::SpritesTextures;
                    self.deferred_cursor = 0;
                    return true;
                }
                DeferredEntityStage::SpritesTextures => {
                    let sprites = lodestone_render::entity_sprite::ENTITY_SPRITES;
                    let Some(sprite) = sprites.get(self.deferred_cursor) else {
                        self.deferred_stage = DeferredEntityStage::Shadow;
                        self.deferred_cursor = 0;
                        continue;
                    };
                    if !budget.take() {
                        return false;
                    }
                    let texture = self
                        .deferred_manager
                        .as_ref()
                        .and_then(|manager| load_image(manager, sprite.texture, "entity sprite"))
                        .map(|image| {
                            let view = entity_texture_from_image(device, queue, &image);
                            self.pipeline.texture_bind_group(device, &view, &self.texture_sampler)
                        });
                    self.sprite_textures.push(texture);
                    self.deferred_cursor += 1;
                    return true;
                }
                DeferredEntityStage::Shadow => {
                    if !budget.take() {
                        return false;
                    }
                    self.shadow_texture = self
                        .deferred_manager
                        .as_ref()
                        .and_then(load_shadow_texture_from_manager)
                        .map(|image| {
                            let view = entity_texture_from_image(device, queue, &image);
                            self.pipeline.texture_bind_group(device, &view, &self.texture_sampler)
                        });
                    self.deferred_stage = DeferredEntityStage::Complete;
                    self.deferred_cursor = 0;
                    self.deferred_assets_pending = false;
                    return true;
                }
                DeferredEntityStage::Complete => {
                    self.deferred_assets_pending = false;
                    return false;
                }
            }
        }
    }

    /// Turn every sheet `crate::remote_skins` has finished fetching into a
    /// texture bind group, keyed by its URL.
    ///
    /// Called once per frame from `app::redraw`. Drains the fast hand-off and
    /// polls the retained cache only to recover URLs missing from this
    /// renderer, so a sheet is uploaded exactly once and the per-frame cost is
    /// zero on the normal steady state.
    pub(super) fn install_pending_player_skins(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        let ready = crate::remote_skins::drain_ready();
        let epoch = crate::remote_skins::sheets_epoch();
        let cached = if self.player_skins_epoch != epoch {
            self.player_skins_epoch = epoch;
            crate::remote_skins::cached_sheets()
        } else {
            Vec::new()
        };
        if ready.is_empty() && cached.is_empty() {
            return;
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-player-skin-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        for (url, image) in ready {
            if self.player_skins.contains_key(&url) {
                continue;
            }
            let view = entity_texture_from_image(device, queue, &image);
            let bg = self.pipeline.texture_bind_group(device, &view, &sampler);
            self.player_skins.insert(url, bg);
        }
        // `READY` is the cheap first hand-off, but it is intentionally
        // one-shot. A renderer can be recreated after that queue was drained;
        // `SHEETS` retains the decoded image precisely so this cache can
        // rehydrate without another request or PNG decode. The contains check
        // keeps the normal steady state at zero uploads despite inspecting the
        // small retained set once per frame.
        for (url, image) in cached {
            if self.player_skins.contains_key(&url) {
                continue;
            }
            let view = entity_texture_from_image(device, queue, &image);
            let bg = self.pipeline.texture_bind_group(device, &view, &sampler);
            self.player_skins.insert(url, bg);
        }
    }

    /// The uploaded armour mesh for a slot, if it has geometry.
    pub(super) fn armour_model(&self, slot: ArmourSlot) -> Option<&GpuEntityModel> {
        self.armour_gpu
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, gpu)| gpu)
    }
}

fn load_image(
    manager: &lodestone_assets::ResourceManager,
    path: &str,
    what: &str,
) -> Option<lodestone_assets::Image> {
    let bytes = manager.read(path)?;
    match lodestone_assets::Image::decode_png(&bytes) {
        Ok(image) => Some(image),
        Err(error) => {
            tracing::warn!(target: "assets", "decode {what} {path}: {error}");
            None
        }
    }
}

fn load_entity_image(
    manager: &lodestone_assets::ResourceManager,
    model_name: &str,
) -> Option<lodestone_assets::Image> {
    lodestone_render::entity_texture_candidates(model_name)
        .iter()
        .find_map(|path| load_image(manager, path, "entity sheet"))
}

fn load_reference_image(
    manager: &lodestone_assets::ResourceManager,
    reference: &str,
) -> Option<lodestone_assets::Image> {
    load_image(
        manager,
        &format!("assets/minecraft/textures/{reference}.png"),
        "entity variant sheet",
    )
}

fn variant_references(manager: &lodestone_assets::ResourceManager) -> Vec<String> {
    let mut seen = HashSet::<String>::new();
    let mut references = Vec::new();
    for skin in lodestone_assets::skin::default_skins() {
        if seen.insert(skin.texture.to_owned()) {
            references.push(skin.texture.to_owned());
        }
    }
    for directory in lodestone_render::entity_variant_sheet_dirs() {
        for path in manager.list(directory) {
            if let Some(reference) = lodestone_render::sheet_reference_of(&path)
                && seen.insert(reference.to_owned())
            {
                references.push(reference.to_owned());
            }
        }
    }
    references.sort();
    references
}

fn armour_texture_specs() -> Vec<((&'static str, ArmourLayerType), String)> {
    use lodestone_assets::equipment::{ARMOUR_ASSETS, armour_texture_path};

    let mut seen = HashSet::new();
    let mut specs = Vec::new();
    for asset in ARMOUR_ASSETS {
        for layer_type in [ArmourLayerType::Humanoid, ArmourLayerType::HumanoidLeggings] {
            for layer in asset.layers(layer_type) {
                let key = (layer.texture, layer_type);
                if seen.insert(key) {
                    specs.push((key, armour_texture_path(layer, layer_type)));
                }
            }
        }
    }
    specs
}

fn load_sheep_wool_texture_from_manager(
    manager: &lodestone_assets::ResourceManager,
) -> Option<lodestone_assets::Image> {
    load_image(
        manager,
        "assets/minecraft/textures/entity/sheep/sheep_wool.png",
        "sheep wool sheet",
    )
}

fn load_elytra_texture_from_manager(
    manager: &lodestone_assets::ResourceManager,
) -> Option<lodestone_assets::Image> {
    load_image(manager, lodestone_assets::entity::ELYTRA_TEXTURE_PATH, "elytra sheet")
}

fn load_flame_textures_from_manager(
    manager: &lodestone_assets::ResourceManager,
) -> Option<lodestone_assets::Image> {
    match lodestone_assets::entity_flame::load_combined_flame_texture(manager) {
        Ok(image) => Some(image),
        Err(error) => {
            tracing::warn!(target: "assets", "load combined flame texture: {error}");
            None
        }
    }
}

fn load_orb_texture_from_manager(
    manager: &lodestone_assets::ResourceManager,
) -> Option<lodestone_assets::Image> {
    load_image(
        manager,
        lodestone_render::EXPERIENCE_ORB_TEXTURE,
        "experience orb sheet",
    )
}

fn load_shadow_texture_from_manager(
    manager: &lodestone_assets::ResourceManager,
) -> Option<lodestone_assets::Image> {
    load_image(manager, lodestone_render::SHADOW_TEXTURE, "entity shadow sheet")
}

fn start_trim_decode(
) -> std::sync::mpsc::Receiver<Vec<(lodestone_assets::ResourceLocation, lodestone_assets::Image)>>
{
    let (sender, receiver) = std::sync::mpsc::channel();
    #[cfg(all(not(test), not(target_arch = "wasm32")))]
    {
        let _ = std::thread::Builder::new()
            .name("lodestone-trim-decode".to_owned())
            .spawn(move || {
                let _ = sender.send(decode_trim_images());
            });
    }
    #[cfg(all(not(test), target_arch = "wasm32"))]
    {
        wasm_bindgen_futures::spawn_local(async move {
            crate::platform::relay::sleep(std::time::Duration::ZERO).await;
            let _ = sender.send(decode_trim_images_batched().await);
        });
    }
    #[cfg(test)]
    {
        let _ = sender.send(decode_trim_images());
    }
    receiver
}

fn decode_trim_images() -> Vec<(lodestone_assets::ResourceLocation, lodestone_assets::Image)> {
    let started = crate::platform::Instant::now();
    let images: Vec<_> = load_trim_sprites().into_iter().collect();
    tracing::info!(
        target: "startup_profile",
        phase = "entity_renderer_trim_decode",
        count = images.len(),
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        "trim decode complete"
    );
    images
}

#[cfg(target_arch = "wasm32")]
async fn decode_trim_images_batched() -> Vec<(lodestone_assets::ResourceLocation, lodestone_assets::Image)> {
    use lodestone_assets::atlas_source::{AtlasDefinition, AtlasSource};
    use lodestone_assets::equipment::ARMOUR_ASSETS;
    use lodestone_assets::trim::{
        ARMOR_TRIMS_ATLAS_PATH, TRIM_MATERIALS, TRIM_PATTERNS, trim_sprite_id,
    };

    let started = crate::platform::Instant::now();
    let Some(manager) = crate::resources::vanilla_manager() else {
        return Vec::new();
    };
    let Some(definition) = AtlasDefinition::load_stacked(&manager, ARMOR_TRIMS_ATLAS_PATH) else {
        tracing::warn!(target: "assets", "load armour trims: descriptor missing");
        return Vec::new();
    };

    let mut allowed = HashSet::new();
    for pattern in TRIM_PATTERNS {
        for material in TRIM_MATERIALS {
            for asset in ARMOUR_ASSETS {
                for layer_type in [ArmourLayerType::Humanoid, ArmourLayerType::HumanoidLeggings] {
                    if let Ok(id) = trim_sprite_id(pattern, material, layer_type, asset.id) {
                        allowed.insert(id);
                    }
                }
            }
        }
    }

    let mut out = HashMap::new();
    let mut batches = 0usize;
    let mut max_batch_ms = 0.0f64;
    for source in definition.sources {
        let AtlasSource::PalettedPermutations {
            textures,
            palette_key,
            permutations,
            separator,
        } = source
        else {
            continue;
        };
        for texture in textures {
            let mut permutation_iter = permutations.iter();
            loop {
                let mut permutation = std::collections::BTreeMap::new();
                for _ in 0..TRIM_PERMUTATIONS_PER_BATCH {
                    let Some((suffix, palette)) = permutation_iter.next() else {
                        break;
                    };
                    permutation.insert(suffix.clone(), palette.clone());
                }
                if permutation.is_empty() {
                    break;
                }
                let source = AtlasSource::PalettedPermutations {
                    textures: vec![texture.clone()],
                    palette_key: palette_key.clone(),
                    permutations: permutation,
                    separator: separator.clone(),
                };
                let batch_started = crate::platform::Instant::now();
                let (baked, _) = lodestone_assets::bake_paletted_permutations(&source, &manager);
                for (id, image) in baked {
                    if allowed.contains(&id) {
                        out.insert(id, image);
                    }
                }
                batches += 1;
                max_batch_ms =
                    max_batch_ms.max(batch_started.elapsed().as_secs_f64() * 1000.0);
                crate::platform::relay::sleep(std::time::Duration::ZERO).await;
            }
        }
    }
    let images: Vec<_> = out.into_iter().collect();
    tracing::info!(
        target: "startup_profile",
        phase = "entity_renderer_trim_decode",
        batches,
        count = images.len(),
        max_batch_ms,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        "trim decode complete"
    );
    images
}

fn upload_painting_model(
    device: &wgpu::Device,
    size: lodestone_render::painting::PaintingSize,
) -> Option<GpuEntityModel> {
    let mesh = lodestone_render::painting::painting_mesh(size.width, size.height);
    let (mut vertices, mut indices) = mesh.front;
    let front = lodestone_render::PartRange {
        index_start: 0,
        index_count: indices.len() as u32,
        vertex_start: 0,
        vertex_count: vertices.len() as u32,
    };
    let frame_index_start = indices.len() as u32;
    let frame_vertex_start = vertices.len() as u32;
    indices.extend(mesh.frame.1.iter().map(|index| index + frame_vertex_start));
    vertices.extend(mesh.frame.0);
    let frame = lodestone_render::PartRange {
        index_start: frame_index_start,
        index_count: indices.len() as u32 - frame_index_start,
        vertex_start: frame_vertex_start,
        vertex_count: vertices.len() as u32 - frame_vertex_start,
    };
    GpuEntityModel::upload_parts(device, &vertices, &indices, vec![front, frame])
}

fn upload_orb_model(device: &wgpu::Device) -> Option<GpuEntityModel> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut parts = Vec::new();
    for icon in 0..lodestone_render::EXPERIENCE_ORB_ICON_COUNT {
        let (cell_vertices, cell_indices) = lodestone_render::experience_orb_mesh(icon);
        let vertex_start = u32::try_from(vertices.len()).unwrap_or(0);
        let index_start = u32::try_from(indices.len()).unwrap_or(0);
        indices.extend(cell_indices.iter().map(|index| index + vertex_start));
        let vertex_count = u32::try_from(cell_vertices.len()).unwrap_or(0);
        vertices.extend(cell_vertices);
        parts.push(lodestone_render::PartRange {
            index_start,
            index_count: u32::try_from(indices.len()).unwrap_or(0) - index_start,
            vertex_start,
            vertex_count,
        });
    }
    GpuEntityModel::upload_parts(device, &vertices, &indices, parts)
}

fn upload_sprite_model(device: &wgpu::Device) -> Option<GpuEntityModel> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut parts = Vec::new();
    for sprite in lodestone_render::entity_sprite::ENTITY_SPRITES {
        let (quad_vertices, quad_indices) =
            lodestone_render::entity_sprite::entity_sprite_mesh(sprite);
        let vertex_start = u32::try_from(vertices.len()).unwrap_or(0);
        let index_start = u32::try_from(indices.len()).unwrap_or(0);
        indices.extend(quad_indices.iter().map(|index| index + vertex_start));
        let vertex_count = u32::try_from(quad_vertices.len()).unwrap_or(0);
        vertices.extend(quad_vertices);
        parts.push(lodestone_render::PartRange {
            index_start,
            index_count: u32::try_from(indices.len()).unwrap_or(0) - index_start,
            vertex_start,
            vertex_count,
        });
    }
    GpuEntityModel::upload_parts(device, &vertices, &indices, parts)
}

#[cfg(test)]
mod deferred_tests {
    use super::{
        DEFERRED_WORK_ITEMS_PER_FRAME, DeferredWorkBudget, TRIM_PERMUTATIONS_PER_BATCH,
    };

    #[test]
    fn deferred_work_budget_has_a_hard_per_redraw_limit() {
        let mut budget = DeferredWorkBudget::new();
        for _ in 0..DEFERRED_WORK_ITEMS_PER_FRAME {
            assert!(budget.take());
        }
        assert_eq!(budget.consumed, DEFERRED_WORK_ITEMS_PER_FRAME);
        assert!(!budget.take());
        assert_eq!(budget.consumed, DEFERRED_WORK_ITEMS_PER_FRAME);
    }

    #[test]
    fn trim_decode_yields_after_a_bounded_batch() {
        assert_eq!(TRIM_PERMUTATIONS_PER_BATCH, 4);
    }
}

/// Decode every humanoid-armour sheet 26.2 ships, keyed by
/// `(texture name, layer type)` — the identity `equipment/<asset>.json` gives a
/// layer, and therefore the identity a bind group needs.
///
/// Version-free and **fail-open**: an empty map means no pack was found or no
/// sheet decoded, and armour then simply does not draw. There is no synthetic
/// fallback on purpose — see [`EntityRenderer::armour_textures`].
///
/// # The jar comes from `resources::vanilla_manager`
///
/// This function used to carry its own copy of `resources.rs`'s pack discovery,
/// alongside two more in this file and a fourth in `hud::vanilla_font`, each with a
/// comment saying the right end state was one `pub(crate) fn vanilla_manager()` that
/// everyone called. That happened: all four are gone.
///
/// It is worth knowing why the collapse was not merely tidiness. `vanilla_manager` is
/// the single place that knows the **browser's jar arrives as `fetch`ed bytes** through
/// `crate::platform::assets` rather than as a path, so a surviving copy would have read
/// a path that cannot exist, found nothing, and drawn armourless players in a browser —
/// while every log line still reported success.
#[cfg(test)]
pub(super) fn load_humanoid_armour_textures()
-> HashMap<(&'static str, ArmourLayerType), lodestone_assets::Image> {
    use lodestone_assets::equipment::{ARMOUR_ASSETS, armour_texture_path};
    use lodestone_assets::Image;

    // `crate::resources::vanilla_manager`, not a fourth hand-rolled copy of the pack
    // discovery. See that function: it is the only place that knows the browser's jar
    // arrives as `fetch`ed bytes rather than a path, so a copy here would silently
    // draw armourless players in a browser while reporting success.
    let mut out = HashMap::new();
    let Some(manager) = crate::resources::vanilla_manager() else {
        tracing::warn!(target: "assets", "no vanilla pack for humanoid armour textures");
        return out;
    };

    for asset in ARMOUR_ASSETS {
        for layer_type in [ArmourLayerType::Humanoid, ArmourLayerType::HumanoidLeggings] {
            for layer in asset.layers(layer_type) {
                let key = (layer.texture, layer_type);
                if out.contains_key(&key) {
                    // Leather shares one layer list between both layer types, so
                    // the same (texture, type) pair is reached twice.
                    continue;
                }
                let path = armour_texture_path(layer, layer_type);
                let Some(png) = manager.read(&path) else {
                    tracing::warn!(target: "assets", "missing armour sheet {path}");
                    continue;
                };
                match Image::decode_png(&png) {
                    Ok(img) => {
                        out.insert(key, img);
                    }
                    Err(e) => tracing::warn!(target: "assets", "decode {path}: {e}"),
                }
            }
        }
    }
    tracing::info!(
        target: "assets",
        loaded = out.len(),
        "loaded vanilla humanoid armour sheets"
    );
    out
}

/// Bake every armour-trim sprite out of the vanilla `client.jar`, keyed by
/// `trim_sprite_id`'s `ResourceLocation`.
///
/// `TrimAtlas::load` does the real work — it reads `atlases/armor_trims.json`,
/// palette-swaps each of the eighteen patterns into each of the eleven materials'
/// suffixes for both layer types, and hands back decoded [`Image`]s. This is only
/// the pack discovery plus the key derivation, and it is the **entry point that did
/// not exist**: `lodestone_assets::trim` was complete with zero callers, so a
/// trimmed chestplate rendered as an untrimmed one.
///
/// Empty (and trims silently absent) with no pack, per
/// [`EntityRenderer::trim_textures`].
///
/// [`Image`]: lodestone_assets::Image
pub(super) fn load_trim_sprites() -> HashMap<lodestone_assets::ResourceLocation, lodestone_assets::Image> {
    use lodestone_assets::equipment::ARMOUR_ASSETS;
    use lodestone_assets::trim::{TRIM_MATERIALS, TRIM_PATTERNS, TrimAtlas, trim_sprite_id};

    let mut out = HashMap::new();
    let Some(manager) = crate::resources::vanilla_manager() else {
        return out;
    };
    let atlas = match TrimAtlas::load(&manager) {
        Ok(atlas) => atlas,
        Err(e) => {
            tracing::warn!(target: "assets", "load armour trims: {e}");
            return out;
        }
    };
    // The keys have to be derived the same way the draw site derives them, so
    // both go through `trim_sprite_id`. Walking the armour assets (rather than a
    // fixed suffix list) is what makes `suffix_for`'s per-wearer override —
    // diamond trim on diamond armour is `diamond_darker` — actually reachable.
    for pattern in TRIM_PATTERNS {
        for material in TRIM_MATERIALS {
            for asset in ARMOUR_ASSETS {
                for layer_type in [ArmourLayerType::Humanoid, ArmourLayerType::HumanoidLeggings] {
                    let Ok(id) = trim_sprite_id(pattern, material, layer_type, asset.id) else {
                        continue;
                    };
                    if out.contains_key(&id) {
                        continue;
                    }
                    if let Some(img) = atlas.sprite_for(pattern, material, layer_type, asset.id) {
                        out.insert(id, img.clone());
                    }
                }
            }
        }
    }
    tracing::info!(target: "assets", loaded = out.len(), "loaded vanilla armour trim sprites");
    out
}

#[cfg(test)]
impl EntityRenderer {
    /// Test-only: rebind every mob to the flat [`synthetic_entity_texture`]
    /// placeholder. A texture-correctness gate renders the *same* mob once with
    /// the real jar sheet and once after this call, so the negative control is
    /// baked into the test and cannot rot: whatever the real sheet does that the
    /// placeholder can't (multiple hues on one mob) has to survive this swap
    /// collapsing to a single hue, or the gate reddens.
    pub(super) fn force_synthetic_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-entity-sampler-synthetic"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let names: Vec<&'static str> = self.textures.keys().copied().collect();
        for name in names {
            let view = synthetic_entity_texture(device, queue, name).0;
            let bg = self.pipeline.texture_bind_group(device, &view, &sampler);
            self.textures.insert(name, bg);
        }
    }
}

/// Upload a decoded RGBA8 entity sheet (a real per-mob texture from the jar) as
/// a GPU texture and return its view. The baked entity quads already carry the
/// per-cuboid UVs that address this sheet, so binding the real PNG is all that
/// stands between the placeholder and a recognisable mob skin. The `wgpu`
/// texture is kept alive by the returned view (and, in turn, the bind group),
/// so it is not returned separately.
///
/// `pub(crate)` so the block-entity pass (`gpu/block_entities.rs`) **and** the
/// container screen's inventory avatar (`container/player_preview.rs`) can share
/// it: the `Rgba8UnormSrgb` choice below is the load-bearing part and a second
/// copy would be free to get it wrong, at +48% brightness on every chest pixel.
/// It was `pub(super)` while both callers lived under `gpu/`.
pub(crate) fn entity_texture_from_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &lodestone_assets::Image,
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lodestone-entity-sheet"),
        size: wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // **`_srgb`, like the block atlas.** A vanilla PNG holds gamma-encoded
        // bytes; binding it as plain `Unorm` hands the shader 0.50 where the
        // linear value is 0.21, and an sRGB swapchain then encodes it a second
        // time. Measured at +48% on every mob pixel — enough on its own to make
        // a mob brighter than the brightest sunlit block face.
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &img.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(img.width * 4),
            rows_per_image: Some(img.height),
        },
        wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Build a 2×2 solid-colour RGBA texture for one entity model, tinted
/// deterministically from the model name so distinct mob types are
/// distinguishable on screen. Opaque, so the shader's alpha cutout keeps every
/// texel. Returns the view and the texture (kept alive by the caller).
fn synthetic_entity_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    model_name: &str,
) -> (wgpu::TextureView, wgpu::Texture) {
    let [r, g, b] = model_tint(model_name);
    const N: u32 = 2;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lodestone-entity-synthetic-sheet"),
        size: wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // **`_srgb`, like the block atlas.** A vanilla PNG holds gamma-encoded
        // bytes; binding it as plain `Unorm` hands the shader 0.50 where the
        // linear value is 0.21, and an sRGB swapchain then encodes it a second
        // time. Measured at +48% on every mob pixel — enough on its own to make
        // a mob brighter than the brightest sunlit block face.
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pixels: Vec<u8> = (0..N * N).flat_map(|_| [r, g, b, 255]).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(N * 4),
            rows_per_image: Some(N),
        },
        wgpu::Extent3d {
            width: N,
            height: N,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (view, texture)
}

/// A deterministic, reasonably-separated RGB tint from a model name (FNV-1a over
/// the bytes, spread across channels). Kept bright (each channel ≥ 80) so mobs
/// read against both sky and terrain.
pub(super) fn model_tint(name: &str) -> [u8; 3] {
    let mut h: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        h ^= u32::from(byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    let chan = |shift: u32| -> u8 { 80 + ((h >> shift) as u8 % 176) };
    [chan(0), chan(8), chan(16)]
}
