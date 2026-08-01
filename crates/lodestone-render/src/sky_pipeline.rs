//! GPU-owning half of the sky renderer: four small pipelines (disc, celestial
//! billboards, stars, clouds) and the [`SkyRenderer`] that drives them, built
//! on the pure geometry in [`crate::sky`].
//!
//! # Its own pipelines, deliberately
//!
//! The model shader is already at wgpu's 4-bind-group floor (camera / atlas /
//! palette / anim — see `model_pipeline.rs`'s own docs and `DESIGN.md`), so a
//! sky pass extending it would push a 5th group and crash on any adapter whose
//! `max_bind_groups` is the wgpu default of 4 (this machine's M5 reports 8 and
//! would not catch it). Every pipeline here uses at most 2 groups (camera, and
//! for the textured passes a texture+sampler), well under the floor, and none
//! of them touch the model pipeline's layout at all.
//!
//! # Depth: sidestepped, not fought
//!
//! This project's depth is `[0,1]` DirectX-style (`LessEqual`, lower = nearer)
//! while vanilla is reversed-Z — every ported comparison and bias direction
//! flips, and the sky is drawn at/behind the far plane, exactly where that
//! trap bites first. Rather than getting a depth comparison direction backwards
//! here, every pipeline in this module sets `depth_stencil: None`: **the sky
//! pass takes no depth attachment at all** and must run in its own render pass,
//! before the terrain/entity passes, writing straight into the colour target.
//! It can never occlude anything (there is nothing to compare against) and
//! nothing it draws can be occluded incorrectly by a sign error, because no
//! depth test happens in this pass at all — terrain drawn afterward simply
//! overwrites sky pixels wherever it is present, which is the correct result
//! for a background layer. See [`SkyRenderer::render`] for the exact pass
//! sequencing a caller must follow.
//!
//! # No double quotes in these shaders, ever
//!
//! Shaders live in Rust `r"..."` raw strings; a literal `"` ends the string
//! early and the compiler then parses the remaining WGSL as Rust source,
//! producing errors that point at English words. Every comment below uses
//! backticks instead, on purpose.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::sky::{
    STAR_FIELD_SEED, build_star_field, celestial_quad_positions, celestial_quad_uvs,
    celestial_rotation_matrix, cloud_plane_geometry, moon_phase_index_for_time_of_day,
    quad_indices, sky_color_for_time_of_day, sky_disc_indices, sky_disc_positions,
    star_brightness_for_time_of_day,
};
use lodestone_assets::{CelestialAtlas, ResourceManager, SkyAssetError};

// ---------------------------------------------------------------------------
// Vertex types
// ---------------------------------------------------------------------------

/// A sky-disc / star vertex: world (camera-relative) position plus a baked
/// RGBA colour (both the disc's per-frame sky colour and each star's
/// per-frame brightness are baked into vertex colour rather than carried as
/// extra uniforms — see the module docs on why this pass rebuilds its small
/// vertex buffers every frame instead of adding more bind groups).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct SkyDiscVertex {
    /// Camera-relative world position.
    pub position: [f32; 3],
    /// Baked RGBA colour.
    pub color: [f32; 4],
}

/// A star vertex — same shape as [`SkyDiscVertex`], named separately because
/// it is logically a different draw ("brightness" rather than "sky colour"),
/// even though the vertex layout and shader are shared with the disc.
pub type StarVertex = SkyDiscVertex;

/// A celestial-billboard (sun or moon) vertex: camera-relative position plus
/// an atlas UV.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct CelestialVertex {
    /// Camera-relative world position.
    pub position: [f32; 3],
    /// Normalised UV into the celestial atlas.
    pub uv: [f32; 2],
}

/// A cloud-plane vertex: camera-relative position, a UV into `clouds.png`
/// (tiled via a `Repeat`-address-mode sampler), and a baked tint colour.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct CloudVertex {
    /// Camera-relative world position.
    pub position: [f32; 3],
    /// UV into the (wrapped) cloud texture.
    pub uv: [f32; 2],
    /// Baked RGBA tint.
    pub color: [f32; 4],
}

/// The one uniform every sky pipeline reads: the translation-free sky
/// view-projection (see [`Camera::sky_view_projection`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct SkyCameraUniform {
    view_proj: [[f32; 4]; 4],
}

fn vertex_layout<const N: usize>(
    stride: u64,
    attrs: &'static [wgpu::VertexAttribute; N],
) -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: stride,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: attrs,
    }
}

fn camera_bind_group_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
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
    })
}

fn texture_bind_group_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn texture_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Uploads a plain RGBA8 image as a single-mip texture with the given address
/// mode and filter, no atlas, no mip chain — every texture this module owns
/// is small (a ~130x40px celestial atlas, a 256x256 cloud map) and sampled
/// directly.
fn upload_plain_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
    address_mode: wgpu::AddressMode,
    filter: wgpu::FilterMode,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
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
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * width.max(1)),
            rows_per_image: Some(height.max(1)),
        },
        wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: address_mode,
        address_mode_v: address_mode,
        address_mode_w: address_mode,
        mag_filter: filter,
        min_filter: filter,
        ..Default::default()
    });
    (texture, view, sampler)
}

// ---------------------------------------------------------------------------
// Shared "position + baked colour" shader, used by the disc and star passes.
// ---------------------------------------------------------------------------

const PASSTHROUGH_COLOR_WGSL: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
";

const CELESTIAL_WGSL: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let color = textureSample(atlas_tex, atlas_smp, in.uv);
    // Sun/moon sprites are circular cutouts on a transparent square; anything
    // this translucent is the cutout, not a dim rim, so it is dropped rather
    // than blended (matches vanilla's own hard-edged celestial sprites).
    if color.a < 0.05 {
        discard;
    }
    return color;
}
";

const CLOUD_WGSL: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var cloud_tex: texture_2d<f32>;
@group(1) @binding(1) var cloud_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let sampled = textureSample(cloud_tex, cloud_smp, in.uv);
    // `CloudRenderer.isCellEmpty`: alpha under 10/255 is an empty cell in
    // `clouds.png` — discarding it here is what turns one flat textured quad
    // into the right cloud silhouette with no CPU-side cell meshing.
    if sampled.a < 0.04 {
        discard;
    }
    return vec4<f32>(sampled.rgb * in.color.rgb, in.color.a);
}
";

// ---------------------------------------------------------------------------
// Pipelines
// ---------------------------------------------------------------------------

/// Vanilla's `BlendFunction.OVERLAY` (`.cache/mc/26.2/client-src/com/mojang/blaze3d/pipeline/BlendFunction.java`),
/// which `RenderPipelines.CELESTIAL` and `.STARS` both use
/// (`RenderPipelines.java`): colour is `src.rgb * src.a + dst.rgb` — additive,
/// weighted by the fragment's own alpha, with the destination **not**
/// attenuated (`dst_factor: One`, not `OneMinusSrcAlpha` as ordinary alpha
/// blending would use).
///
/// This is not a stylistic pick — it is why the real `sun.png` doesn't look
/// wrong in vanilla. `environment/celestial/sun.png` in the 26.2 client jar
/// has **no alpha channel at all** (a plain opaque, palette-indexed PNG):
/// most of its 32x32 RGB is a near-black radial falloff around a small bright
/// core, by design, because vanilla only ever *adds* that falloff onto the
/// sky — it never replaces the sky with it. Sampling that same art with
/// ordinary `SrcAlpha`/`OneMinusSrcAlpha` blending (this pipeline's previous
/// setting) replaces the destination outright wherever alpha is 1.0 — i.e.
/// everywhere, since this texture has no transparency at all — painting the
/// whole opaque square as a mostly-black glyph: the reported "solid black,
/// too big" sun (and the same-shaped moon). `CELESTIAL_WGSL`'s discard only
/// ever fires on a texel whose alpha is actually near-zero, which this asset
/// never has; the *blend function*, not the discard, is what keeps the
/// square's dark corners from ever painting solid in vanilla.
const CELESTIAL_BLEND: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::SrcAlpha,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::Zero,
        operation: wgpu::BlendOperation::Add,
    },
};

fn depthless_targets(color_format: wgpu::TextureFormat, blend: Option<wgpu::BlendState>) -> [Option<wgpu::ColorTargetState>; 1] {
    [Some(wgpu::ColorTargetState {
        format: color_format,
        blend,
        write_mask: wgpu::ColorWrites::ALL,
    })]
}

fn build_pipeline(
    device: &wgpu::Device,
    label: &str,
    shader_src: &str,
    bind_group_layouts: &[&wgpu::BindGroupLayout],
    vertex_layout: wgpu::VertexBufferLayout<'static>,
    color_format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader_src.into()),
    });
    let layout_refs: Vec<Option<&wgpu::BindGroupLayout>> = bind_group_layouts.iter().map(|l| Some(*l)).collect();
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &layout_refs,
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(vertex_layout)],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &depthless_targets(color_format, blend),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            front_face: wgpu::FrontFace::Ccw,
            // The sky is seen from inside the dome; culling by winding would
            // drop faces that happen to wind away from an outward normal.
            cull_mode: None,
            ..Default::default()
        },
        // No depth attachment at all — see the module docs on why.
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// The sky-disc pipeline: opaque, position + baked colour.
#[derive(Debug)]
pub struct SkyDiscPipeline {
    pipeline: wgpu::RenderPipeline,
}

impl SkyDiscPipeline {
    /// Builds the sky-disc pipeline, sharing `camera_layout` (group 0) with
    /// every other sky sub-pipeline.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x4,
        ];
        let pipeline = build_pipeline(
            device,
            "lodestone-sky-disc-pipeline",
            PASSTHROUGH_COLOR_WGSL,
            &[camera_layout],
            vertex_layout(std::mem::size_of::<SkyDiscVertex>() as u64, &ATTRS),
            color_format,
            None,
        );
        Self { pipeline }
    }
}

/// The celestial-billboard (sun/moon) pipeline: alpha-tested, textured.
#[derive(Debug)]
pub struct CelestialPipeline {
    pipeline: wgpu::RenderPipeline,
    pub(crate) texture_layout: wgpu::BindGroupLayout,
}

impl CelestialPipeline {
    /// Builds the celestial-billboard pipeline, sharing `camera_layout`
    /// (group 0) with every other sky sub-pipeline and owning its own
    /// texture bind-group layout (group 1) for the celestial atlas.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x2,
        ];
        let texture_layout = texture_bind_group_layout(device, "lodestone-sky-celestial-tex-bgl");
        let pipeline = build_pipeline(
            device,
            "lodestone-sky-celestial-pipeline",
            CELESTIAL_WGSL,
            &[camera_layout, &texture_layout],
            vertex_layout(std::mem::size_of::<CelestialVertex>() as u64, &ATTRS),
            color_format,
            Some(CELESTIAL_BLEND),
        );
        Self {
            pipeline,
            texture_layout,
        }
    }
}

/// The star-field pipeline: [`CELESTIAL_BLEND`] (vanilla's `RenderPipelines.STARS`
/// is the same `BlendFunction.OVERLAY` as `.CELESTIAL`), position + baked
/// brightness colour. Shares [`SkyDiscPipeline`]'s shader and vertex layout,
/// but needs its own blend state (additive over the disc, not opaque).
#[derive(Debug)]
pub struct StarPipeline {
    pipeline: wgpu::RenderPipeline,
}

impl StarPipeline {
    /// Builds the star-field pipeline, sharing `camera_layout` (group 0)
    /// with every other sky sub-pipeline.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x4,
        ];
        let pipeline = build_pipeline(
            device,
            "lodestone-sky-star-pipeline",
            PASSTHROUGH_COLOR_WGSL,
            &[camera_layout],
            vertex_layout(std::mem::size_of::<StarVertex>() as u64, &ATTRS),
            color_format,
            Some(CELESTIAL_BLEND),
        );
        Self { pipeline }
    }
}

/// The cloud-plane pipeline: alpha-tested, textured, tinted.
#[derive(Debug)]
pub struct CloudPipeline {
    pipeline: wgpu::RenderPipeline,
    pub(crate) texture_layout: wgpu::BindGroupLayout,
}

impl CloudPipeline {
    /// Builds the cloud-plane pipeline, sharing `camera_layout` (group 0)
    /// with every other sky sub-pipeline and owning its own texture
    /// bind-group layout (group 1) for the cloud texture.
    #[must_use]
    pub fn new(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        camera_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x2,
            2 => Float32x4,
        ];
        let texture_layout = texture_bind_group_layout(device, "lodestone-sky-cloud-tex-bgl");
        let pipeline = build_pipeline(
            device,
            "lodestone-sky-cloud-pipeline",
            CLOUD_WGSL,
            &[camera_layout, &texture_layout],
            vertex_layout(std::mem::size_of::<CloudVertex>() as u64, &ATTRS),
            color_format,
            None,
        );
        Self {
            pipeline,
            texture_layout,
        }
    }
}

// ---------------------------------------------------------------------------
// SkyRenderer: owns GPU resources for all four passes and drives them from a
// frame's (camera, time_of_day, sky colour).
// ---------------------------------------------------------------------------

/// Everything needed to draw the sky each frame: the four pipelines above, the
/// celestial atlas and cloud textures, and small dynamic vertex buffers
/// rewritten every call (see the module docs on why CPU-side rebuilding is the
/// right tradeoff at this vertex count).
#[derive(Debug)]
pub struct SkyRenderer {
    camera_layout: wgpu::BindGroupLayout,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,

    disc: SkyDiscPipeline,
    celestial: CelestialPipeline,
    star: StarPipeline,
    cloud: CloudPipeline,

    celestial_bind_group: wgpu::BindGroup,
    cloud_bind_group: wgpu::BindGroup,

    sun_uv: [f32; 4],
    moon_uv: [[f32; 4]; 8],
    cloud_size: (u32, u32),

    disc_vbuf: wgpu::Buffer,
    disc_ibuf: wgpu::Buffer,
    disc_index_count: u32,

    celestial_vbuf: wgpu::Buffer,
    celestial_ibuf: wgpu::Buffer,

    star_vbuf: wgpu::Buffer,
    star_ibuf: wgpu::Buffer,
    star_quad_count: u32,

    cloud_vbuf: wgpu::Buffer,
    cloud_ibuf: wgpu::Buffer,
}

/// Half-extent in blocks of the (flat, alpha-tested) cloud plane — see
/// [`crate::sky::cloud_plane_geometry`].
pub const CLOUD_PLANE_HALF_EXTENT: f32 = 768.0;

fn sprite_uv(sprite: &lodestone_assets::AtlasSprite) -> [f32; 4] {
    [
        sprite.uv_min[0],
        sprite.uv_min[1],
        sprite.uv_max[0],
        sprite.uv_max[1],
    ]
}

fn quad_index_buffer(device: &wgpu::Device, label: &str, quads: u32) -> wgpu::Buffer {
    let mut indices = Vec::with_capacity(quads as usize * 6);
    for q in 0..quads {
        let base = q * 4;
        for i in quad_indices() {
            indices.push(base + i);
        }
    }
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    })
}

fn vertex_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl SkyRenderer {
    /// Builds the sky renderer: loads/stitches the celestial atlas and cloud
    /// texture from `manager`, uploads them, and allocates the (fixed-size,
    /// per-frame-rewritten) geometry buffers.
    ///
    /// # Errors
    ///
    /// Returns [`SkyAssetError`] if the sun, a moon phase, or the cloud
    /// texture is missing/undecodable in `manager`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        manager: &ResourceManager,
    ) -> Result<Self, SkyAssetError> {
        let celestial_atlas = CelestialAtlas::build(manager)?;
        let cloud_image = lodestone_assets::load_cloud_texture(manager)?;

        let camera_layout = camera_bind_group_layout(device, "lodestone-sky-camera-bgl");
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-sky-camera-buffer"),
            size: std::mem::size_of::<SkyCameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-sky-camera-bg"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let disc = SkyDiscPipeline::new(device, color_format, &camera_layout);
        let celestial = CelestialPipeline::new(device, color_format, &camera_layout);
        let star = StarPipeline::new(device, color_format, &camera_layout);
        let cloud = CloudPipeline::new(device, color_format, &camera_layout);

        let atlas = celestial_atlas.atlas();
        let (_tex, atlas_view, atlas_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-sky-celestial-atlas",
            atlas.width,
            atlas.height,
            &atlas.rgba,
            wgpu::AddressMode::ClampToEdge,
            wgpu::FilterMode::Linear,
        );
        let celestial_bind_group = texture_bind_group(
            device,
            &celestial.texture_layout,
            "lodestone-sky-celestial-atlas-bg",
            &atlas_view,
            &atlas_sampler,
        );

        // `Nearest`, not `Linear`: `clouds.png` is a hard binary mask (every
        // texel is either fully transparent or fully opaque white — see
        // `load_cloud_texture`'s doc — vanilla's own `CloudRenderer.isCellEmpty`
        // is a per-*cell* boolean, never a partial-coverage float). Linear
        // filtering interpolates transparent-black and opaque-white texels
        // across every cell boundary; `CLOUD_WGSL`'s alpha-test threshold lets
        // the low-but-nonzero-alpha fringe of that interpolation through, and
        // its near-black *colour* (the same fraction of the way from black to
        // white) renders as-is because this pipeline is opaque
        // (`CloudPipeline` has no blend state) — producing exactly the
        // reported "rounded black outline with a gradient drop-off inside".
        // Nearest sampling never produces a partial-coverage texel: every
        // pixel is either the discarded fully-transparent texel or the solid
        // white one, which also reads closer to vanilla's actual per-cell
        // (not per-pixel-sampled) cloud mesh — see `cloud_plane_geometry`'s
        // module docs on that simplification.
        let (_cloud_tex, cloud_view, cloud_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-sky-cloud-texture",
            cloud_image.width,
            cloud_image.height,
            &cloud_image.rgba,
            wgpu::AddressMode::Repeat,
            wgpu::FilterMode::Nearest,
        );
        let cloud_bind_group = texture_bind_group(
            device,
            &cloud.texture_layout,
            "lodestone-sky-cloud-texture-bg",
            &cloud_view,
            &cloud_sampler,
        );

        let sun_uv = celestial_atlas
            .sun_sprite()
            .map(sprite_uv)
            .unwrap_or([0.0, 0.0, 1.0, 1.0]);
        let mut moon_uv = [[0.0, 0.0, 1.0, 1.0]; 8];
        for (i, slot) in moon_uv.iter_mut().enumerate() {
            if let Some(sprite) = celestial_atlas.moon_sprite(i as u8) {
                *slot = sprite_uv(sprite);
            }
        }

        let disc_index_count = sky_disc_indices().len() as u32;
        let disc_vbuf = vertex_buffer(
            device,
            "lodestone-sky-disc-vbuf",
            (10 * std::mem::size_of::<SkyDiscVertex>()) as u64,
        );
        let disc_ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-sky-disc-ibuf"),
            contents: bytemuck::cast_slice(&sky_disc_indices()),
            usage: wgpu::BufferUsages::INDEX,
        });

        // Two celestial billboards (sun + moon), 4 verts each.
        let celestial_vbuf = vertex_buffer(
            device,
            "lodestone-sky-celestial-vbuf",
            (8 * std::mem::size_of::<CelestialVertex>()) as u64,
        );
        let celestial_ibuf = quad_index_buffer(device, "lodestone-sky-celestial-ibuf", 2);

        let star_field = build_star_field(STAR_FIELD_SEED);
        let star_quad_count = star_field.len() as u32;
        let star_vbuf = vertex_buffer(
            device,
            "lodestone-sky-star-vbuf",
            (star_field.len() * 4 * std::mem::size_of::<StarVertex>()) as u64,
        );
        let star_ibuf = quad_index_buffer(device, "lodestone-sky-star-ibuf", star_quad_count);

        let cloud_vbuf = vertex_buffer(
            device,
            "lodestone-sky-cloud-vbuf",
            (4 * std::mem::size_of::<CloudVertex>()) as u64,
        );
        let cloud_ibuf = quad_index_buffer(device, "lodestone-sky-cloud-ibuf", 1);

        Ok(Self {
            camera_layout,
            camera_buffer,
            camera_bind_group,
            disc,
            celestial,
            star,
            cloud,
            celestial_bind_group,
            cloud_bind_group,
            sun_uv,
            moon_uv,
            cloud_size: (cloud_image.width, cloud_image.height),
            disc_vbuf,
            disc_ibuf,
            disc_index_count,
            celestial_vbuf,
            celestial_ibuf,
            star_vbuf,
            star_ibuf,
            star_quad_count,
            cloud_vbuf,
            cloud_ibuf,
        })
    }

    /// The camera bind-group layout every sub-pipeline shares, exposed for a
    /// caller that wants to build a compatible pipeline of its own.
    #[must_use]
    pub fn camera_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.camera_layout
    }

    /// Draws the whole sky (disc, sun, moon, stars, clouds) for one frame.
    ///
    /// **Must run as its own render pass with only a colour attachment (no
    /// depth attachment) — see the module docs.** Call this before the
    /// terrain/entity passes, which then draw over it normally.
    ///
    /// `camera` supplies [`Camera::sky_view_projection`] and the eye position
    /// (for the cloud plane's world-space UV and height); `time_of_day` is the
    /// same day-clock value the rest of the renderer reads from `WorldTime` —
    /// no second clock. `day_sky_color` is the renderer's existing (currently
    /// time-invariant) sky/clear colour, used as this pass's noon endpoint so
    /// wiring this in does not change how noon looks.
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color_view: &wgpu::TextureView,
        camera: &Camera,
        time_of_day: i64,
        day_sky_color: [f32; 3],
    ) {
        let view_proj = camera.sky_view_projection();
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&SkyCameraUniform {
                view_proj: view_proj.to_cols_array_2d(),
            }),
        );

        let sky_color = sky_color_for_time_of_day(time_of_day, day_sky_color);
        let sky_color4 = [sky_color[0], sky_color[1], sky_color[2], 1.0];

        // Disc: top (overhead) only — the below-horizon "dark disc" vanilla
        // draws when the player's eye is under the horizon is not modelled
        // (an unconditional overhead disc already avoids the "look up past
        // the disc into un-drawn clear colour" edge vanilla's dark disc
        // exists to fix, at the cost of not matching being underground with
        // sky access exactly — see the report for this explicit omission).
        let disc_verts: Vec<SkyDiscVertex> = sky_disc_positions(16.0)
            .into_iter()
            .map(|position| SkyDiscVertex {
                position,
                color: sky_color4,
            })
            .collect();
        queue.write_buffer(&self.disc_vbuf, 0, bytemuck::cast_slice(&disc_verts));

        let angle = crate::sky::celestial_angle_for_time_of_day(time_of_day) * std::f32::consts::TAU;
        let moon_angle = angle + std::f32::consts::PI;

        let mut celestial_verts = Vec::with_capacity(8);
        let sun_pos = celestial_quad_positions(angle, crate::sky::SUN_HEIGHT, crate::sky::SUN_SIZE);
        let sun_uv = celestial_quad_uvs(self.sun_uv, false);
        for i in 0..4 {
            celestial_verts.push(CelestialVertex {
                position: sun_pos[i],
                uv: sun_uv[i],
            });
        }
        let moon_index = moon_phase_index_for_time_of_day(time_of_day);
        let moon_rect = self.moon_uv[usize::from(moon_index) % 8];
        let moon_pos =
            celestial_quad_positions(moon_angle, crate::sky::MOON_HEIGHT, crate::sky::MOON_SIZE);
        let moon_uv = celestial_quad_uvs(moon_rect, true);
        for i in 0..4 {
            celestial_verts.push(CelestialVertex {
                position: moon_pos[i],
                uv: moon_uv[i],
            });
        }
        queue.write_buffer(&self.celestial_vbuf, 0, bytemuck::cast_slice(&celestial_verts));

        let star_brightness = star_brightness_for_time_of_day(time_of_day);
        if star_brightness > 0.0 {
            let rotation = celestial_rotation_matrix(angle);
            let color = [star_brightness, star_brightness, star_brightness, star_brightness];
            let mut star_verts = Vec::with_capacity(self.star_quad_count as usize * 4);
            for quad in build_star_field(STAR_FIELD_SEED) {
                for corner in quad {
                    let p = rotation.transform_point3(glam::Vec3::from(corner));
                    star_verts.push(StarVertex {
                        position: p.to_array(),
                        color,
                    });
                }
            }
            queue.write_buffer(&self.star_vbuf, 0, bytemuck::cast_slice(&star_verts));
        }

        let (cloud_pos, cloud_uv) = cloud_plane_geometry(
            camera.position.to_array(),
            time_of_day,
            self.cloud_size.0,
            self.cloud_size.1,
            CLOUD_PLANE_HALF_EXTENT,
        );
        let cloud_tint = {
            // Clouds read a touch darker than the sky itself so they read as
            // solid rather than glowing; reuses the same day/night blend, not
            // a second colour source.
            [sky_color4[0] * 0.9, sky_color4[1] * 0.9, sky_color4[2] * 0.9, 1.0]
        };
        let cloud_verts: Vec<CloudVertex> = (0..4)
            .map(|i| CloudVertex {
                position: cloud_pos[i],
                uv: cloud_uv[i],
                color: cloud_tint,
            })
            .collect();
        queue.write_buffer(&self.cloud_vbuf, 0, bytemuck::cast_slice(&cloud_verts));

        let _ = device; // reserved for a future resize/rebuild path

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-sky-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(&self.disc.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_vertex_buffer(0, self.disc_vbuf.slice(..));
        pass.set_index_buffer(self.disc_ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.disc_index_count, 0, 0..1);

        if star_brightness > 0.0 {
            pass.set_pipeline(&self.star.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.star_vbuf.slice(..));
            pass.set_index_buffer(self.star_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.star_quad_count * 6, 0, 0..1);
        }

        pass.set_pipeline(&self.celestial.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(1, &self.celestial_bind_group, &[]);
        pass.set_vertex_buffer(0, self.celestial_vbuf.slice(..));
        pass.set_index_buffer(self.celestial_ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..12, 0, 0..1);

        pass.set_pipeline(&self.cloud.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(1, &self.cloud_bind_group, &[]);
        pass.set_vertex_buffer(0, self.cloud_vbuf.slice(..));
        pass.set_index_buffer(self.cloud_ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..6, 0, 0..1);
    }
}
