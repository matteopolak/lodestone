//! GPU-owning half of the sky renderer: five small pipelines (disc, celestial
//! billboards, stars, clouds, sunrise/sunset band) and the [`SkyRenderer`] that
//! drives them, built on the pure geometry in [`crate::sky`].
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
use crate::fog::{VoidFog, scale_gamma};
use crate::sky::{
    STAR_FIELD_SEED, SUNRISE_MIN_ALPHA, build_star_field,
    celestial_quad_positions, celestial_quad_uvs, celestial_rotation_matrix, cloud_color_for_time_of_day,
    cloud_plane_geometry, fog_color_for_time_of_day, moon_phase_index_for_time_of_day, quad_indices,
    sky_color_for_time_of_day, sky_disc_indices, sky_disc_positions, star_brightness_for_time_of_day,
    sunrise_fan_indices, sunrise_fan_positions, sunrise_fan_transform, sunrise_fan_vertex_alphas,
    sunrise_sunset_color_for_time_of_day,
};
use lodestone_assets::{CelestialAtlas, ResourceManager, SkyAssetError};

// ---------------------------------------------------------------------------
// Vertex types
// ---------------------------------------------------------------------------

/// A position-plus-baked-colour sky vertex, shared by the star field and the
/// sunrise/sunset band.
///
/// Both bake a per-frame colour into the vertex rather than carrying it as a
/// uniform (each star's brightness, each fan vertex's `vertex_alpha *
/// sunrise_alpha` product), which is why one shader and one vertex layout serve
/// both — see the module docs on why this pass rebuilds its small vertex
/// buffers every frame instead of adding more bind groups.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct SkyVertex {
    /// Camera-relative world position.
    pub position: [f32; 3],
    /// Baked RGBA colour.
    pub color: [f32; 4],
}

/// A star vertex — [`SkyVertex`], named separately because it is logically a
/// different draw ("brightness" rather than "sky colour").
pub type StarVertex = SkyVertex;

/// A sunrise/sunset fan vertex — [`SkyVertex`], with the band's colour and the
/// fan's own centre-to-rim alpha ramp pre-multiplied into `color`.
pub type SunriseVertex = SkyVertex;

/// A sky-disc vertex: camera-relative position plus **both** ends of the
/// horizon gradient.
///
/// The disc carries two colours rather than one because the gradient is
/// evaluated per fragment (see [`crate::sky::SKY_FOG_END_DISTANCE`] for the vanilla
/// derivation and why per-fragment rather than vanilla's per-vertex): the
/// fragment shader needs the sky colour, the fog colour, and the interpolated
/// camera-relative position, and reads the fade distance from a shader `const`.
/// Both colours are identical across all ten vertices — they are attributes
/// rather than a uniform purely to avoid adding a second bind group to a pass
/// whose whole design note is staying far under the 4-group floor.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct SkyDiscVertex {
    /// Camera-relative world position.
    pub position: [f32; 3],
    /// The zenith end of the gradient: the sky colour, linear RGBA.
    pub color: [f32; 4],
    /// The horizon end of the gradient: the fog colour, linear RGBA.
    pub fog_color: [f32; 4],
}

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

/// The sky disc: a per-**fragment** horizon-to-zenith gradient, ported from
/// `assets/minecraft/shaders/core/sky.fsh` + `include/fog.glsl`.
///
/// Vanilla computes `sphericalVertexDistance = length(Position)` in `sky.vsh`,
/// i.e. once per vertex on a ten-vertex fan, and lets the rasteriser
/// interpolate the resulting *fog factor* across triangles hundreds of blocks
/// wide — which is the banding issue #96 names. This interpolates `local_pos`
/// and takes the `length` here instead. See `crate::sky::SKY_FOG_END_DISTANCE`
/// for the full derivation, including why vanilla's second (cylindrical) fog
/// term is provably dead for this geometry and is therefore absent below.
///
/// No double quotes anywhere in here, including comments — see the module docs.
const SKY_DISC_WGSL: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

// `EnvironmentAttributes.SKY_FOG_END_DISTANCE`'s default, in blocks. Kept in
// step with `crate::sky::SKY_FOG_END_DISTANCE` by a unit test rather than by a
// comment.
const SKY_FOG_END: f32 = 512.0;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) fog_color: vec4<f32>,
    @location(2) local_pos: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) fog_color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(position, 1.0);
    out.color = color;
    out.fog_color = fog_color;
    out.local_pos = position;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // `linear_fog_value(sphericalVertexDistance, 0.0, FogSkyEnd)` — the 0.0
    // start means this is a plain normalised distance, so the disc centre (16
    // blocks up) is essentially pure sky colour and its rim (512 blocks out)
    // is pure fog colour.
    let fog_value = clamp(length(in.local_pos) / SKY_FOG_END, 0.0, 1.0);
    // `apply_fog`: mix weighted by the fog colour's own alpha, and the
    // fragment keeps the sky colour's alpha rather than the fog colour's.
    let rgb = mix(in.color.rgb, in.fog_color.rgb, fog_value * in.fog_color.a);
    return vec4<f32>(rgb, in.color.a);
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

/// Vanilla's `BlendFunction.TRANSLUCENT`
/// (`.cache/mc/26.2/client-src/com/mojang/blaze3d/pipeline/BlendFunction.java`),
/// which `RenderPipelines.SUNRISE_SUNSET` uses: ordinary
/// `SrcAlpha`/`OneMinusSrcAlpha` colour blending, with `One`/`OneMinusSrcAlpha`
/// on alpha.
///
/// Note this is *not* the additive `CELESTIAL_BLEND` — the sunrise band tints
/// the sky it covers rather than adding light to it, which is why a warm orange
/// at high alpha replaces the blue horizon instead of turning it white.
///
/// One honest divergence: vanilla blends in **gamma** space (its framebuffer is
/// not colour-managed), while this pass writes linear values into an
/// `*UnormSrgb` target where the hardware blends in linear space. The band's
/// endpoints are therefore exact and its mid-alpha interior is slightly darker
/// than vanilla's. Fixing that would mean changing the format of the whole
/// frame, not this pipeline.
const SUNRISE_BLEND: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::SrcAlpha,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

/// The sky-disc pipeline: opaque, position + the two gradient endpoints
/// ([`SkyDiscVertex`]).
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
        const ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x4,
            2 => Float32x4,
        ];
        let pipeline = build_pipeline(
            device,
            "lodestone-sky-disc-pipeline",
            SKY_DISC_WGSL,
            &[camera_layout],
            vertex_layout(std::mem::size_of::<SkyDiscVertex>() as u64, &ATTRS),
            color_format,
            None,
        );
        Self { pipeline }
    }
}

/// The sunrise/sunset horizon-band pipeline: [`SUNRISE_BLEND`], position +
/// baked colour. Shares [`StarPipeline`]'s shader and vertex layout and differs
/// only in blend state — additive would make the band glow instead of tint.
#[derive(Debug)]
pub struct SunrisePipeline {
    pipeline: wgpu::RenderPipeline,
}

impl SunrisePipeline {
    /// Builds the sunrise/sunset pipeline, sharing `camera_layout` (group 0)
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
            "lodestone-sky-sunrise-pipeline",
            PASSTHROUGH_COLOR_WGSL,
            &[camera_layout],
            vertex_layout(std::mem::size_of::<SunriseVertex>() as u64, &ATTRS),
            color_format,
            Some(SUNRISE_BLEND),
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
    sunrise: SunrisePipeline,

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

    sunrise_vbuf: wgpu::Buffer,
    sunrise_ibuf: wgpu::Buffer,
    sunrise_index_count: u32,
}

/// Half-extent in blocks of the (flat, alpha-tested) cloud plane — see
/// [`crate::sky::cloud_plane_geometry`].
pub const CLOUD_PLANE_HALF_EXTENT: f32 = 768.0;

/// Everything the sky pass needs about *this frame* beyond the camera.
///
/// This replaced a bare `(time_of_day, day_sky_color)` pair once the horizon
/// gradient and void fog gave the pass a second colour and two world-geometry
/// numbers to read. It is a struct rather than four more positional arguments
/// because `day_sky_color` and `day_fog_color` are both `[f32; 3]` in linear
/// RGB and are trivially swappable at a call site — which would show up as a
/// horizon gradient running the wrong way, not as a compile error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyFrame {
    /// The same day clock the rest of the renderer reads from `WorldTime`. No
    /// second clock, ever — see [`crate::sky`]'s module docs.
    pub time_of_day: i64,
    /// The **linear** RGB base sky colour at noon: the zenith end of the disc's
    /// gradient before the `SKY_COLOR` track's day/night multiplier.
    ///
    /// In vanilla this is the standing biome's `minecraft:visual/sky_color`, and
    /// **it is now that** on a live server: the shell resolves the biome under
    /// the camera and passes its colour through
    /// [`crate::fog::FogSettings::sky_color`]. `#96` is closed.
    ///
    /// A caller with no biome to offer passes the same colour as
    /// [`day_fog_color`](Self::day_fog_color), which every `FogSettings`
    /// constructor does by default — one flat colour for both ends of the
    /// gradient, distinguished only by the two day/night tracks. That is the
    /// pre-#96 behaviour and it is byte-identical to it.
    ///
    /// # Two blockers were recorded here and both were stale; keep the shape
    ///
    /// This doc has now been wrong twice about why the tint was impossible, and
    /// both times it read as perfectly sound:
    ///
    /// 1. It said biome ids "arrive as datapack-registry indices in the
    ///    configuration-phase `registry_data` packet, **which this client does
    ///    not decode**". True when written; #288 landed that ingest within the
    ///    hour.
    /// 2. It then said the decoded **names** had no caller across the
    ///    version-free seam, and that the fix was to carry them on `Login` and
    ///    look the colour up in a jar-derived table — "four edits". The missing
    ///    link was real. The patch was not: `Login` is constructed at 17 sites
    ///    across four protocol families, and this struct's `day_sky_color` was
    ///    fed from `RenderState::clear`, which the shell sets from
    ///    `FogSettings::color` — so the disc centre and the horizon were *the
    ///    same value by construction* and no protocol work upstream could have
    ///    created somewhere for a tint to enter.
    ///
    /// What landed instead: **the colours travel, not the names.** The server
    /// elides nothing (we claim no known packs), so every biome entry arrives
    /// with its full NBT; `ClientRegistries::biome_sky_colors()` lifts one
    /// attribute out, indexed by holder id, and `ClientEvent::BiomeVisuals`
    /// carries the table. No name → colour table exists anywhere, which is what
    /// makes a data pack that reorders the registry, renames a biome or changes a
    /// colour all come out right. See `docs/sky-and-air-bubbles.md`.
    ///
    /// # A warning for whoever changes the gates: pick the two biomes from the data
    ///
    /// Of 66 biome files in
    /// `.cache/mc/26.2/client-src/data/minecraft/worldgen/biome`, 56 declare
    /// `minecraft:visual/sky_color` and they hold only **16 distinct values**.
    /// `plains` and `swamp` are both `#78a7ff` — *byte-identical*, so the
    /// obvious-looking "plains versus swamp" discriminator is vacuous by
    /// construction and would pass against a hardcoded constant. The overworld
    /// spread is genuinely slight (`#6eb1ff` desert/savanna through `#859dff`
    /// frozen peaks; blue is a constant `0xff`). The one dramatic outlier is
    /// `pale_garden` at **`#b9b9b9`**, a desaturated grey.
    ///
    /// That survey is now confirmed against the wire — `live_registry_data.rs`
    /// checks all 66 of a real server's entries against Mojang's own files — and
    /// the gates in `tests/sky_gradient_pixels.rs` use `pale_garden` vs `desert`
    /// for the gross case and `desert` vs `frozen_peaks` for the slight one.
    /// `control_plains_and_swamp_cannot_discriminate` **asserts** the vacuous
    /// pair's zero, so that trap is a measured fact rather than this warning.
    pub day_sky_color: [f32; 3],
    /// The **linear** RGB base fog colour at noon: the horizon end of the
    /// disc's gradient, before the `FOG_COLOR` track's multiplier. Pass the
    /// same colour as the renderer's distance fog so the horizon and the
    /// terrain edge dissolve into each other rather than meeting at a seam.
    pub day_fog_color: [f32; 3],
    /// Where the world bottom is and how far up the void darkening reaches.
    /// [`VoidFog::DISABLED`] to turn it off.
    pub void_fog: VoidFog,
}

impl SkyFrame {
    /// A frame with the fog colour equal to the sky colour and no void fog —
    /// the pass's pre-#96 behaviour, for a caller that has only ever had one
    /// sky colour to give.
    #[must_use]
    pub fn new(time_of_day: i64, day_sky_color: [f32; 3]) -> Self {
        Self {
            time_of_day,
            day_sky_color,
            day_fog_color: day_sky_color,
            void_fog: VoidFog::DISABLED,
        }
    }

    /// Sets the horizon end of the gradient (see
    /// [`day_fog_color`](Self::day_fog_color)).
    #[must_use]
    pub fn with_fog_color(mut self, day_fog_color: [f32; 3]) -> Self {
        self.day_fog_color = day_fog_color;
        self
    }

    /// Sets the void-fog geometry (see [`void_fog`](Self::void_fog)).
    #[must_use]
    pub fn with_void_fog(mut self, void_fog: VoidFog) -> Self {
        self.void_fog = void_fog;
        self
    }

    /// The four colours this frame resolves to, in linear RGB: `(sky, fog,
    /// cloud, sunrise_rgba)`.
    ///
    /// Pure, GPU-free and public so a gate can assert what the pass *will*
    /// paint without a device — and so the composition order (timeline track,
    /// then void-fog darkening, both in gamma space) lives in exactly one
    /// place rather than being repeated in the render body.
    ///
    /// Void fog darkens the sky and the fog but **not** the sunrise band:
    /// vanilla's darkening lives in `FogRenderer.computeFogColor`, which
    /// produces the value `sky.fsh` mixes toward, and never touches
    /// `SUNRISE_SUNSET_COLOR`. (Below the world there is no horizon band
    /// visible anyway — but a gate that measured the band while standing in the
    /// void would otherwise be measuring void fog.)
    #[must_use]
    pub fn resolve_colors(&self, eye_y: f32) -> ([f32; 3], [f32; 3], [f32; 3], [f32; 4]) {
        let brightness = self.void_fog.brightness(eye_y);
        let sky = scale_gamma(
            sky_color_for_time_of_day(self.time_of_day, self.day_sky_color),
            brightness,
        );
        let fog = scale_gamma(
            fog_color_for_time_of_day(self.time_of_day, self.day_fog_color),
            brightness,
        );
        // Clouds are tinted a touch darker than their own track so they read as
        // solid rather than glowing, the same 0.9 the pass has always applied.
        let cloud = cloud_color_for_time_of_day(self.time_of_day, self.day_sky_color)
            .map(|c| c * 0.9);
        let [r, g, b, a] = sunrise_sunset_color_for_time_of_day(self.time_of_day);
        let sunrise = [
            crate::fog::srgb_to_linear_f32(f32::from(r) / 255.0),
            crate::fog::srgb_to_linear_f32(f32::from(g) / 255.0),
            crate::fog::srgb_to_linear_f32(f32::from(b) / 255.0),
            f32::from(a) / 255.0,
        ];
        (sky, fog, cloud, sunrise)
    }
}

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
        let sunrise = SunrisePipeline::new(device, color_format, &camera_layout);

        let atlas = celestial_atlas.atlas();
        let (_tex, atlas_view, atlas_sampler) = upload_plain_texture(
            device,
            queue,
            "lodestone-sky-celestial-atlas",
            atlas.width,
            atlas.height,
            &atlas.rgba,
            wgpu::AddressMode::ClampToEdge,
            // Nearest, not Linear — vanilla does not filter these (issue #372).
            //
            // There is no `.mcmeta` anywhere under
            // `assets/minecraft/textures/environment/`, so nothing there sets
            // `blur: true` and every one of those textures takes the
            // nearest-neighbour default. `sun.png` is a small texture stretched
            // across a large quad, so `Linear` spread its edge over many screen
            // pixels in proportion to that magnification — which is why the sun
            // and moon read as soft while the clouds below, already `Nearest`,
            // read as crisp.
            //
            // Two reasons, not one. The second is independent of fidelity: these
            // sprites are *stitched*, so `Linear` samples across sprite
            // boundaries and can bleed one moon phase into the next.
            // `ClampToEdge` does not help — it governs sampling past `[0, 1]`,
            // not between sprites inside an atlas. If any bleed survives this,
            // the sprite rects need half-texel insetting.
            //
            // This was the only `Linear` *magnification* filter in the renderer.
            // Every other sampler here is `Nearest` for mag, including the block
            // atlas (`texture.rs`: `mag: Nearest, min: Linear, mipmap: Linear`,
            // which is correct — minification wants the mip chain). Only
            // `screen_effects.rs` legitimately wants `Linear`, being a blur.
            wgpu::FilterMode::Nearest,
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

        let sunrise_indices = sunrise_fan_indices();
        let sunrise_index_count = sunrise_indices.len() as u32;
        let sunrise_vbuf = vertex_buffer(
            device,
            "lodestone-sky-sunrise-vbuf",
            (crate::sky::SUNRISE_FAN_VERTICES * std::mem::size_of::<SunriseVertex>()) as u64,
        );
        let sunrise_ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-sky-sunrise-ibuf"),
            contents: bytemuck::cast_slice(&sunrise_indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        Ok(Self {
            camera_layout,
            camera_buffer,
            camera_bind_group,
            disc,
            celestial,
            star,
            cloud,
            sunrise,
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
            sunrise_vbuf,
            sunrise_ibuf,
            sunrise_index_count,
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
    /// (for the cloud plane's world-space UV and height, and for void fog);
    /// `frame` carries the day clock and this frame's base colours — see
    /// [`SkyFrame`].
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color_view: &wgpu::TextureView,
        camera: &Camera,
        frame: &SkyFrame,
    ) {
        let time_of_day = frame.time_of_day;
        let view_proj = camera.sky_view_projection();
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&SkyCameraUniform {
                view_proj: view_proj.to_cols_array_2d(),
            }),
        );

        let (sky_color, fog_color, cloud_color, sunrise) =
            frame.resolve_colors(camera.position.y);
        let sky_color4 = [sky_color[0], sky_color[1], sky_color[2], 1.0];
        let fog_color4 = [fog_color[0], fog_color[1], fog_color[2], 1.0];

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
                fog_color: fog_color4,
            })
            .collect();
        queue.write_buffer(&self.disc_vbuf, 0, bytemuck::cast_slice(&disc_verts));

        let angle = crate::sky::celestial_angle_for_time_of_day(time_of_day) * std::f32::consts::TAU;

        // The sunrise/sunset band, between the disc and the celestial bodies —
        // vanilla's own order in `LevelRenderer.addSkyPass` (disc, sunrise,
        // then sun/moon/stars), which matters because the band is translucent
        // and the sun must draw *over* it.
        let sunrise_alpha = sunrise[3];
        let draw_sunrise = sunrise_alpha > SUNRISE_MIN_ALPHA;
        if draw_sunrise {
            let placement = sunrise_fan_transform(angle, sunrise_alpha);
            let positions = sunrise_fan_positions();
            let alphas = sunrise_fan_vertex_alphas();
            let verts: Vec<SunriseVertex> = positions
                .iter()
                .zip(alphas)
                .map(|(p, vertex_alpha)| SunriseVertex {
                    position: placement.transform_point3(glam::Vec3::from(*p)).to_array(),
                    // `core/position_color.fsh` is `vertexColor * ColorModulator`,
                    // i.e. the fan's centre-to-rim ramp times the band colour;
                    // both factors are folded in here.
                    color: [
                        sunrise[0],
                        sunrise[1],
                        sunrise[2],
                        vertex_alpha * sunrise_alpha,
                    ],
                })
                .collect();
            queue.write_buffer(&self.sunrise_vbuf, 0, bytemuck::cast_slice(&verts));
        }

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
        // Resolved by `SkyFrame::resolve_colors` from the real `CLOUD_COLOR`
        // track (not from the sky colour, which is now correctly black at
        // night — see `crate::sky::CLOUD_COLOR_TRACK`).
        let cloud_tint = [cloud_color[0], cloud_color[1], cloud_color[2], 1.0];
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

        if draw_sunrise {
            pass.set_pipeline(&self.sunrise.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.sunrise_vbuf.slice(..));
            pass.set_index_buffer(self.sunrise_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.sunrise_index_count, 0, 0..1);
        }

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
