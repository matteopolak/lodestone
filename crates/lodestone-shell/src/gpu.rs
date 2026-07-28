//! GPU render state for the shell: owns the block pipeline, the atlas, a depth
//! buffer, and a per-section table of uploaded meshes + camera uniforms, and
//! draws them all in one pass.
//!
//! Every section carries its own camera-uniform buffer because the block
//! shader's uniform bundles `view_proj` *with* the section's world origin (the
//! packed vertex only stores a 0..16 local position). Each frame we rewrite all
//! section uniforms with the current `view_proj` *before* opening the render
//! pass — buffers can't be written mid-pass — then issue one draw per section.

use std::collections::HashMap;

use lodestone_assets::ResourceLocation;
use lodestone_render::{
    AnimSlotUniform, BlockAtlas, BlockPipeline, Camera, CameraUniform, DEPTH_FORMAT, DepthBuffer,
    ENTITY_FULLBRIGHT, EntityCameraUniform, EntityModelSet, EntityPipeline, GpuAtlas,
    GpuEntityModel, GpuMesh, GpuModelMesh, ItemGeometry, Mesh, ModelCameraUniform, ModelMesh,
    ModelPipeline, SpriteAnimation,
    block::{camera_buffer, sprite_uv_buffer},
    crack_pipeline::{CrackPipeline, GpuCrackMesh},
    crack_resolver::CrackResolver,
    entity::{dropped_item_mesh, ground_transform_for, item_bob_offset},
    entity_camera_buffer,
    fog::{FogSettings, FogUniform},
    model_anim_buffer, model_camera_buffer, plan_entities, update_model_anim_buffer,
    upload_instances,
    vertex::vram_bytes,
};

use glam::Vec3;

use crate::entities::{EntityDraw, ITEM_ENTITY_TYPE_PATH};
use crate::mesher::{SectionGeometry, SectionKey};
use crate::particles::{ParticleInstance, ParticleRenderer};

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
pub const SKY_COLOR: [f32; 3] = [0.242_867, 0.462_361, 0.827_571];

/// Fraction of the view distance at which fog begins.
///
/// The outer quarter of the render volume is the fade band: near enough that
/// the edge chunks dissolve rather than pop in, far enough that fog is not
/// visible during normal play.
pub const FOG_START_FRACTION: f32 = 0.75;

/// The block currently being mined, for the progressive crack overlay: its world
/// position, vanilla state id (to resolve the block's real model geometry) and
/// destruction stage `0..=9`. Passed to [`RenderState::render_with_crack`].
#[derive(Debug, Clone, Copy)]
pub struct CrackTarget {
    /// World block position of the target.
    pub block: [i32; 3],
    /// Vanilla state id, used to resolve the block's baked quads.
    pub state_id: u32,
    /// Destruction stage `0..=9`; selects the `destroy_stage_N` sprite.
    pub stage: u8,
}

/// The 12 edges of a unit cube as pairs of corner indices (line list).
const CUBE_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (3, 2),
    (2, 0), // bottom face
    (4, 5),
    (5, 7),
    (7, 6),
    (6, 4), // top face
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7), // verticals
];

/// Draws a black wireframe box around the targeted block. Its own pipeline
/// (line-list topology, `LessEqual` depth, no depth write, alpha-blended) so it
/// reads clearly over terrain without a second pass or z-fighting.
#[derive(Debug)]
struct OutlineRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
}

impl OutlineRenderer {
    fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-outline-shader"),
            source: wgpu::ShaderSource::Wgsl(
                r"
struct Uniform { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: Uniform;

@vertex
fn vs_main(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return u.view_proj * vec4<f32>(pos, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.6);
}
"
                .into(),
            ),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-outline-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-outline-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        // 24 vertices (12 edges × 2), 3 f32 each.
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-vertices"),
            size: (24 * 3 * std::mem::size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-outline-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-outline-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (3 * std::mem::size_of::<f32>()) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group,
            uniform,
            vertices,
        }
    }

    /// Upload the view-projection and the box vertices for `block` (slightly
    /// expanded so the lines sit just outside the block faces). Must be called
    /// before the render pass begins — buffers can't be written mid-pass.
    fn prepare(&self, queue: &wgpu::Queue, view_proj: &[[f32; 4]; 4], block: [i32; 3]) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));

        const PAD: f32 = 0.002;
        let lo = [
            block[0] as f32 - PAD,
            block[1] as f32 - PAD,
            block[2] as f32 - PAD,
        ];
        let hi = [
            block[0] as f32 + 1.0 + PAD,
            block[1] as f32 + 1.0 + PAD,
            block[2] as f32 + 1.0 + PAD,
        ];
        // Corner index bit layout: x = bit0, y = bit1, z = bit2.
        let corner = |i: usize| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        let mut verts = [0f32; 24 * 3];
        for (e, &(a, b)) in CUBE_EDGES.iter().enumerate() {
            let ca = corner(a);
            let cb = corner(b);
            let base = e * 6;
            verts[base..base + 3].copy_from_slice(&ca);
            verts[base + 3..base + 6].copy_from_slice(&cb);
        }
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&verts));
    }

    fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..24, 0..1);
    }
}

/// Aggregate numbers for one rendered frame, surfaced to the debug overlay.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStats {
    /// Sections with non-empty geometry drawn this frame.
    pub sections_drawn: usize,
    /// Total merged quads across all drawn sections.
    pub total_quads: usize,
    /// Draw calls issued (one per non-empty section).
    pub draw_calls: usize,
    /// Approximate mesh VRAM in bytes.
    pub vram_bytes: usize,
    /// Entity instances drawn this frame (post-frustum-cull).
    pub entities_drawn: usize,
    /// Entity instances frustum-culled this frame.
    pub entities_culled: usize,
    /// Particle billboards drawn this frame.
    pub particles_drawn: usize,
    /// Dropped-item entities drawn this frame (item entities with a known stack
    /// *and* baked geometry). Distinct from `entities_drawn`, which counts only
    /// the cuboid-rig mobs the entity pipeline handles — an item entity never
    /// appears there, so without this counter a frame full of drops is
    /// indistinguishable from an empty one.
    pub item_drops_drawn: usize,
}

#[derive(Debug)]
struct SectionGpu {
    mesh: GpuMesh,
    quad_count: usize,
    origin: [f32; 3],
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
}

/// One uploaded section of wide baked-model geometry (the vanilla path). Mirrors
/// [`SectionGpu`] but holds a [`GpuModelMesh`] and draws through the
/// [`ModelPipeline`].
#[derive(Debug)]
struct ModelSectionGpu {
    /// Opaque block geometry (with lava merged in), if any.
    mesh: Option<GpuModelMesh>,
    quad_count: usize,
    /// Translucent water surface geometry for this section, if any. Drawn on the
    /// fluid pass after all opaque geometry so the sea floor shows through.
    water: Option<GpuModelMesh>,
    water_quad_count: usize,
    origin: [f32; 3],
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
}

/// GPU resources for the model render pass: the model pipeline, the complete
/// stitched block atlas it samples (distinct from the packed cube atlas — its
/// UVs are what the baked quads index), and a per-section table of uploaded
/// model meshes. Present only on the live vanilla path; `None` on the demo path,
/// which meshes full cubes through the packed [`BlockPipeline`].
#[derive(Debug)]
struct ModelRenderer {
    pipeline: ModelPipeline,
    /// The translucent fluid pipeline (no cutout discard, water tint, alpha
    /// blend, depth-test on / depth-write off). Shares the model camera and
    /// atlas bind groups.
    water_pipeline: ModelPipeline,
    #[allow(dead_code)]
    atlas: GpuAtlas,
    atlas_bind_group: wgpu::BindGroup,
    /// The tint palette (group 2) uploaded once: one RGBA multiplier per palette
    /// index, resolved from the pack's real colormaps. The model shader looks it
    /// up per tinted quad so grass, foliage and every other source get their own
    /// colour instead of one hardcoded green.
    palette_bind_group: wgpu::BindGroup,
    /// The buffer behind [`Self::palette_bind_group`], kept so other consumers of
    /// the model shader — the HUD's 3-D item pass — can build their **own** bind
    /// group over the *same* palette rather than uploading a second copy. A
    /// hotbar icon and the world block it depicts then cannot drift apart.
    palette_buffer: wgpu::Buffer,
    /// The animated block sprites' timelines paired with each slot's normalised
    /// frame height, cloned from the block models so the per-slot animation
    /// uniform can be rebuilt from the current game tick each frame via
    /// [`RenderState::update_animation`]. Ordered by slot id (entry `i` is slot
    /// `i + 1`); empty when the pack has no animated block sprites.
    animations: Vec<(SpriteAnimation, f32)>,
    /// The per-slot animation uniform buffer (one [`AnimSlotUniform`] per slot,
    /// slot 0 static). Rewritten each frame from the game tick; both shaders
    /// sample it to offset an animated quad's V into its current frame.
    anim_buffer: wgpu::Buffer,
    /// The animation bind group for the opaque model pipeline (its group 3).
    anim_bind_group: wgpu::BindGroup,
    /// The animation bind group for the fluid pipeline (its group 2). Wraps the
    /// same [`Self::anim_buffer`]; only the group index differs.
    water_anim_bind_group: wgpu::BindGroup,
    /// The mining-crack overlay pipeline (alpha-blended, depth-test only, pulled
    /// toward the camera by a negative depth bias so the `destroy_stage` texels
    /// win the depth test against the coplanar block face without z-fighting).
    crack_pipeline: CrackPipeline,
    /// Per-state baked quads + the ten `destroy_stage` rects, captured from the
    /// block models so the target block's crack mesh can be built at draw time
    /// after `BlockModels` itself is dropped. Follows the block's real geometry
    /// (slabs, stairs, crosses), never a synthetic full cube.
    crack_resolver: CrackResolver,
    /// The crack pass's atlas bind group. The crack pipeline has its own bind
    /// group layout, so it needs its own bind group over the same stitched
    /// model atlas the opaque pass uses.
    crack_atlas_bind_group: wgpu::BindGroup,
    /// The crack pass's camera buffer + bind group. Crack meshes carry
    /// world-space positions (section origin zero), rewritten with the current
    /// `view_proj` each frame like the section uniforms.
    crack_cam_buffer: wgpu::Buffer,
    crack_cam_bind_group: wgpu::BindGroup,
    /// Baked inventory geometry for every item that has some, snapshotted here
    /// while `BlockModels` is still borrowable (exactly as
    /// [`CrackResolver::from_models`] snapshots the per-state quads, and for the
    /// same reason: the atlas is dropped after construction, so a per-frame
    /// borrow is not available).
    ///
    /// This is what lets a dropped item be drawn from inside
    /// [`RenderState::render`] with **no** new argument threaded through
    /// `app.rs`: the geometry is already here, and the only thing a frame has to
    /// supply is which item each drop is carrying, which rides on
    /// [`EntityDraw::item`].
    items: HashMap<ResourceLocation, ItemGeometry>,
    /// The dropped-item pass's camera buffer + bind group. Item drops are meshed
    /// with **world** positions baked in (the spin and bob are folded into the
    /// vertex positions, not an instance matrix), so this carries the plain
    /// view-projection with a zero section origin, like the crack pass's.
    drop_cam_buffer: wgpu::Buffer,
    drop_cam_bind_group: wgpu::BindGroup,
    sections: HashMap<SectionKey, ModelSectionGpu>,
}

/// Build the per-slot animation uniform array for game `tick` from the snapshot
/// of animated sprite timelines. Index 0 is the static sentinel; index `s`
/// (`1..=len`) is slot `s`, its sampled region resolved into a V offset by the
/// slot's normalised frame height. Always yields at least the sentinel, so the
/// uniform buffer is never zero-sized.
fn anim_slots_at(animations: &[(SpriteAnimation, f32)], tick: u64) -> Vec<AnimSlotUniform> {
    let mut slots = Vec::with_capacity(animations.len() + 1);
    slots.push(AnimSlotUniform::static_slot());
    for (animation, frame_v) in animations {
        slots.push(AnimSlotUniform::from_sample(
            animation.sample(tick),
            *frame_v,
        ));
    }
    slots
}

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
struct EntityRenderer {
    pipeline: EntityPipeline,
    models: EntityModelSet,
    gpu_models: HashMap<&'static str, GpuEntityModel>,
    textures: HashMap<&'static str, wgpu::BindGroup>,
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
}

impl EntityRenderer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, color_format: wgpu::TextureFormat) -> Self {
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

        Self {
            pipeline,
            models,
            gpu_models,
            textures,
            cam_buffer,
            cam_bind_group,
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
    fn force_synthetic_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
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
fn entity_texture_from_image(
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
fn model_tint(name: &str) -> [u8; 3] {
    let mut h: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        h ^= u32::from(byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    let chan = |shift: u32| -> u8 { 80 + ((h >> shift) as u8 % 176) };
    [chan(0), chan(8), chan(16)]
}

/// Samples the world's packed sky/block light (`sky << 4 | block`) at an
/// entity's feet, so a mob is lit by the block it stands in exactly as vanilla
/// lights it (`LivingEntityRenderer` → `Level::getLightColor`, one sample per
/// entity).
///
/// Only the shell's `Sim` owns a world to sample, and `RenderState` is handed
/// pre-interpolated `EntityDraw`s with no light on them, so this is the seam
/// between the two. Unset — the offline demo, a headless test — every mob is
/// [`ENTITY_FULLBRIGHT`], which is the behaviour before entity lighting existed.
///
/// The `Fn` is boxed rather than a `fn` pointer because a real sampler has to
/// capture the client handle; the manual [`Debug`] keeps `RenderState`'s derive
/// working.
#[derive(Default)]
pub struct EntityLightSource(Option<Box<dyn Fn(Vec3) -> Option<u8> + Send + Sync>>);

impl EntityLightSource {
    /// Packed light at `feet`, or [`ENTITY_FULLBRIGHT`] when there is no sampler
    /// or the position is outside loaded chunks. A `None` here is deliberately
    /// **not** darkness: an unloaded neighbour should not black out a mob, the
    /// same call the particle path makes (`Sim::extract_particles`).
    #[must_use]
    fn sample(&self, feet: Vec3) -> u8 {
        self.0
            .as_ref()
            .and_then(|f| f(feet))
            .unwrap_or(ENTITY_FULLBRIGHT)
    }
}

impl std::fmt::Debug for EntityLightSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EntityLightSource")
            .field(&if self.0.is_some() { "set" } else { "full-bright" })
            .finish()
    }
}

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
    entities: EntityRenderer,
    /// Block-break debris. Bound to whichever atlas the terrain draws from, so a
    /// fragment is textured from the same pixels as the block it came off.
    particles: ParticleRenderer,
    particle_atlas_bind_group: wgpu::BindGroup,
    clear: wgpu::Color,
    /// Linear distance fog fading the outermost loaded chunks into the sky (or,
    /// later, a biome water colour when submerged). Defaults to a sky-coloured
    /// fog sized for the default render distance; drive it from the real render
    /// distance / eye-in-fluid state via [`RenderState::set_fog`].
    fog: FogSettings,
    /// How each mob's world light is sampled. Full-bright until the shell wires
    /// a real world in via [`RenderState::set_entity_light_source`].
    entity_light: EntityLightSource,
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
        let entities = EntityRenderer::new(device, queue, color_format);

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
            let crack_cam_bind_group =
                crack_pipeline.camera_bind_group(device, &crack_cam_buffer);
            // Dropped items: snapshot the baked item geometry and build the
            // pass's own world-space camera buffer, both while `models` is still
            // in scope.
            let items: HashMap<ResourceLocation, ItemGeometry> = models
                .items()
                .map(|(id, geometry)| (id.clone(), geometry.clone()))
                .collect();
            let drop_cam_buffer = model_camera_buffer(
                device,
                CameraUniform {
                    view_proj: [[0.0; 4]; 4],
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
            );
            let drop_cam_bind_group = pipeline.camera_bind_group(device, &drop_cam_buffer);
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
                drop_cam_buffer,
                drop_cam_bind_group,
                sections: HashMap::new(),
            }
        });

        // Particles sample the same atlas the terrain does. The two atlases are
        // disjoint UV spaces (the packed cube atlas vs the complete baked-model
        // atlas), so binding the wrong one throws correctly-shaped debris in
        // some other block's colours.
        let particles = ParticleRenderer::new(device, color_format);
        let particle_atlas = model.as_ref().map_or(&atlas, |m| &m.atlas);
        let particle_atlas_bind_group =
            particles.atlas_bind_group(device, &particle_atlas.view, &particle_atlas.sampler);

        Self {
            pipeline,
            atlas,
            uv_buffer,
            atlas_bind_group,
            depth,
            sections: HashMap::new(),
            model,
            outline,
            entities,
            particles,
            particle_atlas_bind_group,
            // Full-bright until the shell installs a world sampler; see
            // `set_entity_light_source`.
            entity_light: EntityLightSource::default(),
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

    /// Replace the distance-fog settings (colour + range). The shell drives this
    /// from its configured render distance and the eye-in-fluid state: a
    /// sky-coloured fog sized to the render distance normally, a short
    /// biome-coloured water fog when submerged. Pass [`FogSettings::disabled`]
    /// to turn fog off.
    pub fn set_fog(&mut self, fog: FogSettings) {
        self.fog = fog;
    }

    /// Install the world light sampler mobs are lit by (see
    /// [`EntityLightSource`]). Call once, after a world exists; without it every
    /// mob renders [`ENTITY_FULLBRIGHT`] and out-shines the terrain it stands on
    /// at night.
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
                // A section may carry only opaque terrain, only water (an ocean
                // surface section with no solid blocks), or both. Drop it only
                // when neither has geometry.
                if opaque_gpu.is_none() && water_gpu.is_none() {
                    model.sections.remove(&key);
                    return;
                }
                // Placeholder uniform; overwritten every frame with the live camera.
                let cam_buffer = model_camera_buffer(
                    device,
                    CameraUniform {
                        view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                        section_origin: [origin_f[0], origin_f[1], origin_f[2], 0.0],
                    },
                );
                let cam_bind_group = model.pipeline.camera_bind_group(device, &cam_buffer);
                model.sections.insert(
                    key,
                    ModelSectionGpu {
                        mesh: opaque_gpu,
                        quad_count: opaque.quad_count(),
                        water: water_gpu,
                        water_quad_count: water.quad_count(),
                        origin: origin_f,
                        cam_buffer,
                        cam_bind_group,
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
        if let Some(model) = self.model.as_mut() {
            model.sections.remove(key);
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
        self.render_inner(device, queue, view, camera, outline, entities, None)
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
        self.render_inner(device, queue, view, camera, outline, entities, Some(crack))
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

        // Same for the model sections (live vanilla path). Fog is folded into
        // the group-0 uniform: the eye position (for per-fragment view
        // distance) and this frame's fog settings travel with each section's
        // camera buffer, keeping the model shader within four bind groups.
        if let Some(model) = &self.model {
            let eye = camera.position;
            let fog = FogUniform::new(&self.fog, [eye.x, eye.y, eye.z]);
            for section in model.sections.values() {
                let uniform = ModelCameraUniform {
                    camera: CameraUniform {
                        view_proj,
                        section_origin: [
                            section.origin[0],
                            section.origin[1],
                            section.origin[2],
                            0.0,
                        ],
                    },
                    fog,
                };
                queue.write_buffer(&section.cam_buffer, 0, bytemuck::bytes_of(&uniform));
            }
        }

        // Outline vertices/uniform must be written before the pass opens.
        if let Some(block) = outline {
            self.outline.prepare(queue, &view_proj, block);
        }

        // Resolve, frustum-cull and upload entity instances *before* the pass —
        // buffers can't be created mid-pass, and the entity camera uniform (no
        // section origin; the world position lives in each instance matrix) must
        // be written first too.
        let mut stats = RenderStats::default();
        let entity_batches = self.prepare_entities(device, queue, camera, entities, &mut stats);

        // Dropped items, meshed and uploaded before the pass for the same reason
        // as everything else here (no buffer creation mid-pass).
        let item_drop_mesh = self.prepare_item_drops(device, queue, camera, entities, &mut stats);

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
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("block pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear),
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
                    pass.set_bind_group(0, &section.cam_bind_group, &[]);
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

            if let Some(model) = &self.model {
                // Dropped items, through the *model* pipeline rather than the
                // entity one: an item entity is an item model, not a cuboid
                // rig. Same atlas / palette / animation bind groups as terrain,
                // so a dropped block is textured from exactly the pixels the
                // placed block is. Opaque and depth-writing, drawn alongside the
                // mobs and before translucent water for the same reason they
                // are (see the entity note above).
                if let Some(mesh) = &item_drop_mesh {
                    pass.set_pipeline(&model.pipeline.pipeline);
                    pass.set_bind_group(0, &model.drop_cam_bind_group, &[]);
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
                    pass.set_bind_group(0, &section.cam_bind_group, &[]);
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

            if outline.is_some() {
                self.outline.draw(&mut pass);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));

        stats.vram_bytes = vram_bytes(stats.total_quads);
        stats
    }

    /// Mesh this frame's dropped items into one world-space [`GpuModelMesh`],
    /// and rewrite the drop pass's camera uniform.
    ///
    /// Returns `None` — and draws nothing — when there is no vanilla model pass,
    /// when no tracked entity is an item, or when no item entity has both a
    /// known stack and baked geometry. That last case is vanilla's own
    /// behaviour: `ItemEntityRenderer.submit` returns immediately on an empty
    /// stack.
    ///
    /// # One mesh, not one per drop
    ///
    /// Each drop's bob and spin are folded into its **vertex positions** by
    /// [`dropped_item_mesh`], so unlike the mobs there is no per-instance matrix
    /// to batch on and no shared geometry between two drops of different items.
    /// Concatenating them into a single buffer is therefore both the simplest
    /// and the cheapest option: one upload and one draw call per frame however
    /// many items are on the ground, versus one of each per drop.
    fn prepare_item_drops(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera: &Camera,
        entities: &[EntityDraw],
        stats: &mut RenderStats,
    ) -> Option<GpuModelMesh> {
        let model = self.model.as_ref()?;
        let frustum = camera.frustum();
        let mut combined = ModelMesh::default();
        for draw in entities {
            if draw.type_path != ITEM_ENTITY_TYPE_PATH {
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
            let ground = ground_transform_for(geometry.gui_light);
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
        let eye = camera.position;
        queue.write_buffer(
            &model.drop_cam_buffer,
            0,
            bytemuck::bytes_of(&ModelCameraUniform {
                camera: CameraUniform {
                    view_proj: camera.view_projection().to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::new(&self.fog, [eye.x, eye.y, eye.z]),
            }),
        );
        Some(mesh)
    }

    /// Resolve each interpolated entity into a renderable instance, frustum-cull
    /// and group them by model, upload one instance buffer per surviving model,
    /// and record draw/cull counts. Runs before the render pass so every GPU
    /// buffer it creates outlives the pass that reads it.
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
        // lives per-instance, so the section origin stays zero) **and this
        // frame's fog**, from the same `self.fog` the terrain sections get. Both
        // passes therefore fade on one curve; a mob under water or at the render
        // edge dissolves with the blocks around it instead of punching through.
        let eye = camera.position;
        queue.write_buffer(
            &self.entities.cam_buffer,
            0,
            bytemuck::bytes_of(&EntityCameraUniform {
                camera: CameraUniform {
                    view_proj: camera.view_projection().to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::new(&self.fog, [eye.x, eye.y, eye.z]),
            }),
        );

        let instances: Vec<_> = entities
            .iter()
            .filter_map(|e| {
                self.entities
                    .models
                    .resolve(&e.type_path, e.feet, e.yaw, e.scale, &e.anim)
                    .map(|i| i.with_light(self.entity_light.sample(e.feet)))
            })
            .collect();

        let frame = plan_entities(&instances, &camera.frustum());
        stats.entities_drawn = frame.stats.drawn;
        stats.entities_culled = frame.stats.culled_frustum;

        // One instance buffer per *part*, not per entity: the mesh's vertices are
        // part-local, so a limb only moves if its own matrices are uploaded
        // separately. A mob is ~10–35 parts but hundreds of quads, so this moves
        // roughly 1% of the data a per-entity vertex re-bake would.
        frame
            .batches
            .iter()
            .map(|batch| {
                let count = u32::try_from(batch.transforms.len()).unwrap_or(u32::MAX);
                // Every part uploads the *same* light slice: a mob's lightmap
                // sample is per entity, so its head and its leg share one value.
                let parts = batch
                    .parts
                    .iter()
                    .map(|p| upload_instances(device, p, &batch.lights))
                    .collect();
                EntityDrawBatch {
                    model: batch.model,
                    count,
                    parts,
                }
            })
            .collect()
    }
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

    /// The sky reference must stay a plausible blue in the readback's own space;
    /// a value that drifted to the *displayed* colour would blow the "is this
    /// pixel sky?" test open, which is exactly how the two gates below broke.
    #[test]
    fn sky_reference_tracks_the_clear_colour() {
        assert_eq!(sky_clear_bytes(), [62, 118, 211]);
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
                id: 1,
                type_path: "pig".to_owned(),
                item: None,
                feet: pig_feet,
                yaw: 0.0,
                head_yaw: 0.0,
                pitch: 0.0,
                scale: 1.0,
                anim: lodestone_render::AnimInput::REST,
            },
            // A second pig behind the camera so frustum culling has something
            // real to remove — the anti-vacuity guard on the cull path.
            EntityDraw {
                id: 2,
                type_path: "pig".to_owned(),
                item: None,
                feet: glam::Vec3::new(0.0, 0.0, -12.0),
                yaw: 0.0,
                head_yaw: 0.0,
                pitch: 0.0,
                scale: 1.0,
                anim: lodestone_render::AnimInput::REST,
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
            let corner = (x < w / 8 || x >= 7 * w / 8) && (y < h / 8 || y >= 7 * h / 8);
            if corner && is_mob(px) {
                corner_px += 1;
            }
        }
        let coverage = mob_px as f64 / (w * h) as f64;

        eprintln!("=== shell entity render (headless) ===");
        eprintln!("entities drawn  = {}", stats.entities_drawn);
        eprintln!("entities culled = {}", stats.entities_culled);
        eprintln!("mob coverage    = {:.2}%", coverage * 100.0);
        eprintln!("centre mob px   = {centre_px}");
        eprintln!("corner mob px   = {corner_px}");

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
            id: 1,
            type_path: "zombie".to_owned(),
            item: None,
            feet: glam::Vec3::new(0.0, 0.0, 3.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
        }];

        // Fraction of a mob's bright pixels whose *hue direction* is far from the
        // model's single flat placeholder tint. Brightness scaling (lighting)
        // leaves the direction unchanged, so under the placeholder this is ~0; a
        // real multi-hue sheet pushes it up.
        let off_hue_fraction = |pixels: &[u8]| -> (usize, f64) {
            let sky = sky_clear_bytes().map(f32::from);
            let tint = model_tint("zombie");
            let tv = glam::Vec3::new(tint[0] as f32, tint[1] as f32, tint[2] as f32).normalize();
            let mut mob = 0usize;
            let mut off = 0usize;
            for px in pixels.chunks_exact(4) {
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
