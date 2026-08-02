//! GPU resources and texture loading for the entity render pass: mobs,
//! humanoid armour layers, and the sheep wool layer.
use std::collections::HashMap;

use lodestone_assets::equipment::{ArmourLayerType, ArmourSlot};
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
    pub(super) pipeline: EntityPipeline,
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
    /// The sheep wool layer (issue #53): the one baked mesh on the CPU (needed
    /// per frame to pair each part with the wearer's own part index, the same
    /// as `armour_models`), its GPU upload, and its one texture bind group.
    ///
    /// Drawn through the **base** entity pipeline (`self.pipeline`, `Less`),
    /// not `armour_pipeline` (`LessEqual`) — wool has no second layer at the
    /// same inflation as itself, so there is no coplanar z-fighting to correct
    /// for the way leather's dyeable base and overlay need. See
    /// `docs/entity-rendering.md`.
    ///
    /// `wool_texture` is `None` without a vanilla pack, and wool then draws
    /// nothing rather than falling back to a synthetic colour — the same
    /// asymmetry `armour_textures` documents, for the same reason: a
    /// flat-coloured fleece shell reads as a rendering bug.
    pub(super) wool_models: SheepWoolModelSet,
    pub(super) wool_gpu: Option<GpuEntityModel>,
    pub(super) wool_texture: Option<wgpu::BindGroup>,
}

impl EntityRenderer {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        let pipeline = EntityPipeline::new(device, color_format);
        let models = EntityModelSet::load();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-entity-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let mut gpu_models = HashMap::new();
        let mut textures = HashMap::new();
        // Real per-mob sheets from client.jar, keyed by model name. Empty (and so
        // every model falls back to a synthetic placeholder) when no pack is
        // present — e.g. the offline demo world or a headless test.
        let real = crate::resources::load_entity_textures();
        for (name, mesh) in models.iter() {
            if let Some(gpu) = GpuEntityModel::upload(device, mesh) {
                gpu_models.insert(name, gpu);
            }
            let view = match real.get(name) {
                Some(img) => entity_texture_from_image(device, queue, img),
                None => synthetic_entity_texture(device, queue, name).0,
            };
            let bg = pipeline.texture_bind_group(device, &view, &sampler);
            textures.insert(name, bg);
        }

        // The armour layers. Four meshes, uploaded once and shared by every
        // material — the geometry depends only on the slot's inflation, so
        // eight materials do not mean eight helmets.
        let armour_pipeline = pipeline.armour_pipeline(device, color_format);
        let armour_models = ArmourModelSet::load();
        let armour_gpu: Vec<(ArmourSlot, GpuEntityModel)> = armour_models
            .iter()
            .filter_map(|(slot, mesh)| {
                GpuEntityModel::upload_armour(device, mesh).map(|gpu| (slot, gpu))
            })
            .collect();
        let armour_textures: HashMap<(&'static str, ArmourLayerType), wgpu::BindGroup> =
            load_humanoid_armour_textures()
                .iter()
                .map(|(key, img)| {
                    let view = entity_texture_from_image(device, queue, img);
                    (*key, pipeline.texture_bind_group(device, &view, &sampler))
                })
                .collect();

        // The sheep wool layer. One mesh, uploaded once — unlike armour, wool
        // has no per-material variant to multiply it by.
        let wool_models = SheepWoolModelSet::load();
        let wool_gpu = GpuEntityModel::upload_wool(device, wool_models.mesh());
        let wool_texture = load_sheep_wool_texture().map(|img| {
            let view = entity_texture_from_image(device, queue, &img);
            pipeline.texture_bind_group(device, &view, &sampler)
        });

        // A persistent group-0 uniform, rewritten every frame before the pass.
        // Sized for camera **plus fog**: the entity shader reads both out of one
        // binding, so a buffer sized for the camera alone would leave the fog
        // block reading past the end.
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

        // The arm pass's own group-0 uniform (see the field docs). Fog is
        // disabled rather than shared: the arm sits ~0.7 blocks from the eye and
        // the nearest fog onset any preset produces is lava's 0 (and the sky
        // fog's is `render_distance * 16 - clamp(that / 10, 4, 64)`, i.e. 115.2
        // blocks at the default render distance — issue #388 replaced a flat
        // 0.75× fraction with vanilla's span, but either way the arm is orders of
        // magnitude nearer than the ramp), so a shared fog block could only ever
        // contribute rounding — and vanilla likewise does not fog the hand.
        // The *sky darken* lane is still rewritten each frame, because a
        // permanently noon-lit arm over a dark world is exactly the "mobs are
        // super bright at night" defect in miniature.
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

        Self {
            pipeline,
            models,
            gpu_models,
            textures,
            cam_buffer,
            cam_bind_group,
            hand_cam_buffer,
            hand_cam_bind_group,
            armour_pipeline,
            armour_models,
            armour_gpu,
            armour_textures,
            wool_models,
            wool_gpu,
            wool_texture,
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

/// Decode every humanoid-armour sheet 26.2 ships, keyed by
/// `(texture name, layer type)` — the identity `equipment/<asset>.json` gives a
/// layer, and therefore the identity a bind group needs.
///
/// Version-free and **fail-open**: an empty map means no pack was found or no
/// sheet decoded, and armour then simply does not draw. There is no synthetic
/// fallback on purpose — see [`EntityRenderer::armour_textures`].
///
/// # This duplicates `resources.rs`'s pack discovery, and should not have to
///
/// `resources::asset_root`/`open_client_jar` are private and
/// `resources::vanilla_manager` is `#[cfg(test)]`, so production code in another
/// module cannot reach any of them; `crate::hud::vanilla_font::jar_manager`
/// already carries an identical copy for exactly this reason and says so. The
/// right end state is one `pub(crate) fn vanilla_manager()` in `resources.rs`
/// with all three callers going through it — a one-line attribute change in a
/// file this pass does not own. Until then the discovery rule is duplicated
/// *exactly*: `LODESTONE_ASSETS` if set and complete, else the highest-sorting
/// `.cache/mc/<version>` under any ancestor of the working directory holding
/// both `client.jar` and `generated/reports/blocks.json`.
pub(super) fn load_humanoid_armour_textures()
-> HashMap<(&'static str, ArmourLayerType), lodestone_assets::Image> {
    use lodestone_assets::equipment::{ARMOUR_ASSETS, armour_texture_path};
    use lodestone_assets::{Image, ResourceManager, ResourceSource, ZipSource};
    use std::path::{Path, PathBuf};

    fn is_pack_root(dir: &Path) -> bool {
        dir.join("client.jar").is_file() && dir.join("generated/reports/blocks.json").is_file()
    }
    fn pack_root() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("LODESTONE_ASSETS") {
            let p = PathBuf::from(dir);
            return is_pack_root(&p).then_some(p);
        }
        let cwd = std::env::current_dir().ok()?;
        for base in cwd.ancestors() {
            let mut entries: Vec<PathBuf> = match std::fs::read_dir(base.join(".cache/mc")) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| is_pack_root(p))
                    .collect(),
                Err(_) => continue,
            };
            entries.sort();
            if let Some(root) = entries.pop() {
                return Some(root);
            }
        }
        None
    }

    let mut out = HashMap::new();
    let Some(jar) = pack_root().map(|root| root.join("client.jar")) else {
        return out;
    };
    let Ok(bytes) = std::fs::read(&jar) else {
        tracing::warn!(target: "assets", "read {}", jar.display());
        return out;
    };
    let Ok(zip) = ZipSource::from_bytes(bytes) else {
        tracing::warn!(target: "assets", "open {}", jar.display());
        return out;
    };
    let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);

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

/// Decode the sheep wool layer's own sheet (`entity/sheep/sheep_wool.png`)
/// from the vanilla `client.jar`, or `None` if no pack is found — the wool
/// equivalent of [`load_humanoid_armour_textures`], with the same duplicated
/// pack-discovery rationale documented there (`resources::vanilla_manager` is
/// `#[cfg(test)]`-only, so production code in this module cannot reach it).
///
/// Confirmed 64×32 and exactly greyscale against the real jar by
/// `lodestone-assets/tests/real_jar.rs::sheep_wool_texture_decodes_from_the_real_jar`
/// — that is why [`sheep_wool_tint`] can paint this sheet with a flat gamma-
/// space multiply rather than needing a per-colour texture.
fn load_sheep_wool_texture() -> Option<lodestone_assets::Image> {
    use lodestone_assets::{Image, ResourceManager, ResourceSource, ZipSource};
    use std::path::{Path, PathBuf};

    fn is_pack_root(dir: &Path) -> bool {
        dir.join("client.jar").is_file() && dir.join("generated/reports/blocks.json").is_file()
    }
    fn pack_root() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("LODESTONE_ASSETS") {
            let p = PathBuf::from(dir);
            return is_pack_root(&p).then_some(p);
        }
        let cwd = std::env::current_dir().ok()?;
        for base in cwd.ancestors() {
            let mut entries: Vec<PathBuf> = match std::fs::read_dir(base.join(".cache/mc")) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| is_pack_root(p))
                    .collect(),
                Err(_) => continue,
            };
            entries.sort();
            if let Some(root) = entries.pop() {
                return Some(root);
            }
        }
        None
    }

    let jar = pack_root()?.join("client.jar");
    let bytes = std::fs::read(&jar)
        .inspect_err(|_| tracing::warn!(target: "assets", "read {}", jar.display()))
        .ok()?;
    let zip = ZipSource::from_bytes(bytes)
        .inspect_err(|_| tracing::warn!(target: "assets", "open {}", jar.display()))
        .ok()?;
    let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);
    const PATH: &str = "assets/minecraft/textures/entity/sheep/sheep_wool.png";
    let Some(png) = manager.read(PATH) else {
        tracing::warn!(target: "assets", "missing sheep wool sheet {PATH}");
        return None;
    };
    match Image::decode_png(&png) {
        Ok(img) => Some(img),
        Err(e) => {
            tracing::warn!(target: "assets", "decode {PATH}: {e}");
            None
        }
    }
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
/// `pub(super)` so the block-entity pass (`gpu/block_entities.rs`) can share it:
/// the `Rgba8UnormSrgb` choice below is the load-bearing part and a second copy
/// would be free to get it wrong, at +48% brightness on every chest pixel.
pub(super) fn entity_texture_from_image(
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
