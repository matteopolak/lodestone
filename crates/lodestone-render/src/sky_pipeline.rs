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
//! # The shaders are files, not string literals
//!
//! `src/shaders/sky_{disc,celestial,cloud,passthrough_color}.wgsl`, pulled in
//! with `include_str!`. They used to be Rust `r"..."` raw strings, where a
//! literal double quote — even inside a WGSL comment — ended the string early
//! and rustc then parsed the remaining WGSL and English prose as Rust,
//! producing errors that pointed at ordinary words. That trap is gone rather
//! than documented; write comments here normally. See `docs/shaders.md`.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::cloud_mesh::CloudCells;
use crate::fog::{VoidFog, scale_gamma};
use crate::sky::{
    CLOUD_FANCY_RADIUS_CELLS, CloudStatus, STAR_FIELD_SEED, SUNRISE_MIN_ALPHA, build_star_field,
    celestial_quad_positions, celestial_quad_uvs, celestial_rotation_matrix, cloud_color_for_time_of_day,
    cloud_fancy_max_faces, cloud_plane_geometry, fancy_cloud_geometry, fog_color_for_time_of_day,
    moon_phase_index_for_time_of_day, quad_indices, sky_color_for_time_of_day, sky_disc_indices,
    sky_disc_positions, star_brightness_for_time_of_day, sunrise_fan_indices, sunrise_fan_positions,
    sunrise_fan_transform, sunrise_fan_vertex_alphas, sunrise_sunset_color_for_time_of_day,
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

/// A sky-disc vertex: camera-relative position, **both** ends of the horizon
/// gradient, and the distance the gradient takes to get there.
///
/// The disc carries two colours rather than one because the gradient is
/// evaluated per fragment (see [`crate::sky::SKY_FOG_END_DISTANCE`] for the vanilla
/// derivation and why per-fragment rather than vanilla's per-vertex): the
/// fragment shader needs the sky colour, the fog colour, the interpolated
/// camera-relative position, and the fade distance.
/// All three non-positional attributes are identical across all ten vertices —
/// they are attributes rather than a uniform purely to avoid adding a second bind
/// group to a pass whose whole design note is staying far under the 4-group
/// floor. `fog_end` joined them for the same reason in issue #399, when it
/// stopped being a shader `const`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct SkyDiscVertex {
    /// Camera-relative world position.
    pub position: [f32; 3],
    /// The zenith end of the gradient: the sky colour, linear RGBA.
    pub color: [f32; 4],
    /// The horizon end of the gradient: the fog colour, linear RGBA.
    pub fog_color: [f32; 4],
    /// Distance in blocks at which the gradient has reached
    /// [`fog_color`](Self::fog_color) outright — vanilla's `fog.skyEnd`, i.e.
    /// [`crate::sky::sky_fog_end_for_render_distance`]. Not a constant: see
    /// [`SkyFrame::sky_fog_end`].
    pub fog_end: f32,
}

/// [`SkyDiscVertex`]'s vertex attributes, at module scope rather than inside
/// [`SkyDiscPipeline::new`] so that
/// `the_disc_vertex_layout_covers_the_whole_vertex` checks **the array the
/// pipeline is built from** instead of a copy of it. A copy would agree with
/// itself while a field added to the struct went unread by the shader.
const DISC_ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
    0 => Float32x3,
    1 => Float32x4,
    2 => Float32x4,
    3 => Float32,
];

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

const PASSTHROUGH_COLOR_WGSL: &str = include_str!("shaders/sky_passthrough_color.wgsl");

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
const SKY_DISC_WGSL: &str = include_str!("shaders/sky_disc.wgsl");

const CELESTIAL_WGSL: &str = include_str!("shaders/sky_celestial.wgsl");

const CLOUD_WGSL: &str = include_str!("shaders/sky_cloud.wgsl");

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

/// Vanilla's `BlendFunction.TRANSLUCENT`, which `CLOUDS_SNIPPET` uses for both
/// cloud pipelines (`RenderPipelines.java:106-113`): ordinary
/// `SrcAlpha`/`OneMinusSrcAlpha` compositing.
///
/// The cloud pipeline was **opaque** (`blend: None`), which made
/// [`crate::sky::CLOUD_COLOR_ALPHA`]'s `0.8` inert — the fragment's alpha was
/// written to the target and never used to weight anything, so clouds painted
/// solid. Vanilla's clouds are 80% white over the sky, which is most of why they
/// read as *cloud*. Note this is deliberately not [`CELESTIAL_BLEND`]: the sun
/// and moon are additive (`dst_factor: One`) because their art is a near-black
/// falloff meant to be added; clouds replace what they cover, in proportion.
const CLOUD_BLEND: wgpu::BlendState = wgpu::BlendState::ALPHA_BLENDING;

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
        let pipeline = build_pipeline(
            device,
            "lodestone-sky-disc-pipeline",
            SKY_DISC_WGSL,
            &[camera_layout],
            vertex_layout(std::mem::size_of::<SkyDiscVertex>() as u64, &DISC_ATTRS),
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

/// The cloud-plane pipeline: alpha-tested, textured, tinted, and **blended**
/// (see [`CLOUD_BLEND`]).
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
            Some(CLOUD_BLEND),
        );
        Self {
            pipeline,
            texture_layout,
        }
    }
}

/// The FANCY cloud pipeline: [`CLOUD_BLEND`], untextured position + baked
/// colour — the same vertex layout and shader as [`StarPipeline`]/
/// [`SunrisePipeline`] (see [`SkyVertex`]), because vanilla's FANCY clouds are
/// shaded per-face by a fixed colour table
/// (`rendertype_clouds.vsh`'s `faceColors`), not sampled from a texture at
/// all — the texture only decides *which cells are filled*, which
/// `crate::cloud_mesh`/`crate::sky::fancy_cloud_geometry` resolve on the CPU
/// before a single vertex reaches the GPU. No texture bind group, unlike
/// [`CloudPipeline`]'s FAST quad.
#[derive(Debug)]
pub struct FancyCloudPipeline {
    pipeline: wgpu::RenderPipeline,
}

impl FancyCloudPipeline {
    /// Builds the FANCY cloud pipeline, sharing `camera_layout` (group 0)
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
            "lodestone-sky-fancy-cloud-pipeline",
            PASSTHROUGH_COLOR_WGSL,
            &[camera_layout],
            vertex_layout(std::mem::size_of::<SkyVertex>() as u64, &ATTRS),
            color_format,
            Some(CLOUD_BLEND),
        );
        Self { pipeline }
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
    fancy_cloud: FancyCloudPipeline,
    sunrise: SunrisePipeline,

    celestial_bind_group: wgpu::BindGroup,
    cloud_bind_group: wgpu::BindGroup,

    sun_uv: [f32; 4],
    moon_uv: [[f32; 4]; 8],
    cloud_size: (u32, u32),
    /// The FANCY cell grid, voxelized once from `clouds.png` at construction
    /// (`crate::cloud_mesh::CloudCells::from_rgba`) — the texture never
    /// changes at runtime, so there is nothing to rebuild here across frames,
    /// only the face list [`fancy_cloud_geometry`] walks over it.
    cloud_cells: CloudCells,

    disc_vbuf: wgpu::Buffer,
    disc_ibuf: wgpu::Buffer,
    disc_index_count: u32,

    celestial_vbuf: wgpu::Buffer,
    celestial_ibuf: wgpu::Buffer,

    star_vbuf: wgpu::Buffer,
    star_ibuf: wgpu::Buffer,
    /// The **unrotated** star quads, built once here and rotated per frame —
    /// which is what [`crate::sky::build_star_field`]'s own doc asks for and what
    /// vanilla does (it rotates a static star buffer by `starAngle` rather than
    /// rebuilding it).
    ///
    /// `render` used to call `build_star_field` again on every night frame and
    /// keep only the length here, dropping the `Vec` — ~1500 iterations of four
    /// `SplitMix64` draws plus an allocation, per frame, for a pure function of a
    /// fixed seed. Keeping the geometry rather than the count also removes the
    /// only way the buffer capacity and the draw could disagree: the quad count
    /// is now `star_base.len()` at both.
    star_base: Vec<[[f32; 3]; 4]>,

    cloud_vbuf: wgpu::Buffer,
    cloud_ibuf: wgpu::Buffer,

    /// Sized for [`CLOUD_FANCY_RADIUS_CELLS`] via
    /// [`crate::sky::cloud_fancy_max_faces`] — the real per-frame face count
    /// (`fancy_cloud_geometry`'s output) is always `<=` this, so the buffer
    /// never needs to grow; [`SkyRenderer::render`] draws only the real count.
    fancy_cloud_vbuf: wgpu::Buffer,
    fancy_cloud_ibuf: wgpu::Buffer,
    fancy_cloud_max_faces: u32,

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
    /// Distance in blocks over which the disc runs from
    /// [`day_sky_color`](Self::day_sky_color) to
    /// [`day_fog_color`](Self::day_fog_color) — vanilla's `fog.skyEnd`.
    ///
    /// **This is a function of the player's render distance, not a constant**
    /// (issue #399). Set it with
    /// [`with_render_distance`](Self::with_render_distance), which applies
    /// `AtmosphericFogEnvironment.java:73`'s
    /// `min(renderDistanceInBlocks, SKY_FOG_END_DISTANCE)`. It is *not* the
    /// disc's radius, which stays [`crate::sky::SKY_DISC_RADIUS`]: shortening
    /// this saturates the outer part of the same disc to the fog colour rather
    /// than shrinking the geometry, so the horizon end stays the fog colour and
    /// only the number of degrees of elevation the ramp occupies changes.
    ///
    /// [`SkyFrame::new`] leaves it at [`crate::sky::SKY_FOG_END_DISTANCE`], which
    /// is the attribute's registered default and therefore correct at render
    /// distance 32 and above. A caller that knows the render distance and does not
    /// say so gets a gradient stretched by `512 / (rd_chunks * 16)`, which at the
    /// client default of 8 chunks is 4x — the whole of #399. Prefer
    /// `with_render_distance` at every site that has the number.
    pub sky_fog_end: f32,
    /// FAST (one alpha-tested quad) or FANCY (real extruded per-cell
    /// geometry) — vanilla's own `CloudStatus` option, minus `OFF` (see
    /// [`CloudStatus`]'s doc for why there is nowhere for a player to have
    /// chosen that yet). Defaults to `Fancy`, matching vanilla's own default.
    pub cloud_status: CloudStatus,
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
            sky_fog_end: crate::sky::SKY_FOG_END_DISTANCE,
            cloud_status: CloudStatus::default(),
        }
    }

    /// Sets [`cloud_status`](Self::cloud_status) — FAST or FANCY clouds.
    #[must_use]
    pub fn with_cloud_status(mut self, cloud_status: CloudStatus) -> Self {
        self.cloud_status = cloud_status;
        self
    }

    /// Sets the horizon end of the gradient (see
    /// [`day_fog_color`](Self::day_fog_color)).
    #[must_use]
    pub fn with_fog_color(mut self, day_fog_color: [f32; 3]) -> Self {
        self.day_fog_color = day_fog_color;
        self
    }

    /// Sets [`sky_fog_end`](Self::sky_fog_end) from the player's render distance
    /// in chunks, via [`crate::sky::sky_fog_end_for_render_distance`] — the
    /// builder every caller that knows the render distance should use (issue
    /// #399).
    #[must_use]
    pub fn with_render_distance(mut self, render_distance_chunks: u32) -> Self {
        self.sky_fog_end = crate::sky::sky_fog_end_for_render_distance(render_distance_chunks);
        self
    }

    /// Sets [`sky_fog_end`](Self::sky_fog_end) in blocks directly, already
    /// clamped to [`crate::sky::SKY_FOG_END_DISTANCE`].
    ///
    /// For a caller holding a view distance in blocks rather than chunks (and for
    /// gates that want to state the distance outright). Prefer
    /// [`with_render_distance`](Self::with_render_distance) when the chunk count
    /// is what is actually known, so the `* 16` lives in one place.
    #[must_use]
    pub fn with_sky_fog_end(mut self, sky_fog_end_blocks: f32) -> Self {
        self.sky_fog_end = crate::sky::sky_fog_end_for_render_distance_blocks(sky_fog_end_blocks);
        self
    }

    /// Sets the void-fog geometry (see [`void_fog`](Self::void_fog)).
    #[must_use]
    pub fn with_void_fog(mut self, void_fog: VoidFog) -> Self {
        self.void_fog = void_fog;
        self
    }

    /// The four colours this frame resolves to, in linear RGB: `(sky, fog,
    /// cloud_rgba, sunrise_rgba)`.
    ///
    /// The cloud entry carries **alpha**, because vanilla's cloud colour is
    /// `ARGB.white(0.8F)` and the `0.8` is the whole reason clouds read as cloud
    /// rather than as paint — see [`crate::sky::CLOUD_COLOR_ALPHA`].
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
    pub fn resolve_colors(&self, eye_y: f32) -> ([f32; 3], [f32; 3], [f32; 4], [f32; 4]) {
        let brightness = self.void_fog.brightness(eye_y);
        let sky = scale_gamma(
            sky_color_for_time_of_day(self.time_of_day, self.day_sky_color),
            brightness,
        );
        let fog = scale_gamma(
            fog_color_for_time_of_day(self.time_of_day, self.day_fog_color),
            brightness,
        );
        // Vanilla's cloud colour is the `CLOUD_COLOR` attribute — **pure white**
        // at alpha 0.8 for the overworld (`ARGB.white(0.8F)`) — multiplied by the
        // `CLOUD_COLOR` timeline track. It is *not* derived from the sky colour.
        //
        // This used to pass `day_sky_color` as the base and then scale the result
        // by an invented `0.9` ("so they read as solid rather than glowing"),
        // which made a noon cloud `#78A7FF × 0.9`: the reported blue-grey. Both
        // the base and the `0.9` are gone. Void fog does not touch clouds —
        // `FogRenderer.computeFogColor` darkens the fog colour, and the cloud
        // attribute is read straight off the probe in `LevelExtractor.java:202`.
        let cloud_rgb = cloud_color_for_time_of_day(self.time_of_day, crate::sky::CLOUD_COLOR_RGB);
        let cloud = [
            cloud_rgb[0],
            cloud_rgb[1],
            cloud_rgb[2],
            crate::sky::CLOUD_COLOR_ALPHA,
        ];
        let [r, g, b, a] = sunrise_sunset_color_for_time_of_day(self.time_of_day);
        let sunrise = [
            crate::fog::srgb_to_linear_f32(f32::from(r) / 255.0),
            crate::fog::srgb_to_linear_f32(f32::from(g) / 255.0),
            crate::fog::srgb_to_linear_f32(f32::from(b) / 255.0),
            f32::from(a) / 255.0,
        ];
        (sky, fog, cloud, sunrise)
    }

    /// The colour the sky pass's colour target must be cleared to for this
    /// frame — **this frame's resolved fog colour**, in linear RGB.
    ///
    /// Everything the finite overhead disc does not cover keeps this colour, so
    /// it is what the player sees below the horizon wherever terrain does not
    /// reach. See [`SkyRenderer::render`]'s `clear` parameter for the full
    /// account and for vanilla's equivalent (`LevelRenderer`'s `"clear"` pass at
    /// `fogColor`).
    ///
    /// It reads [`resolve_colors`](Self::resolve_colors) rather than
    /// `day_fog_color` for two reasons, and both have bitten this file before:
    /// the `FOG_COLOR` track darkens the horizon at night (a day-colour clear
    /// would paint a bright band under a near-black night sky), and void fog
    /// darkens it underground. Reading the *resolved* colour also makes the
    /// clear equal the disc's own rim colour by construction — the disc paints
    /// `mix(sky, fog, 1.0)` at and beyond `sky_fog_end`, which is exactly `fog`
    /// — so the horizon cannot band. `the_clear_colour_is_the_discs_own_rim`
    /// asserts that identity.
    #[must_use]
    pub fn clear_color(&self, eye_y: f32) -> [f32; 3] {
        self.resolve_colors(eye_y).1
    }

    /// [`clear_color`](Self::clear_color) as an opaque `wgpu::Color`, ready to
    /// hand straight to [`SkyRenderer::render`].
    #[must_use]
    pub fn clear_color_wgpu(&self, eye_y: f32) -> wgpu::Color {
        let [r, g, b] = self.clear_color(eye_y);
        wgpu::Color {
            r: f64::from(r),
            g: f64::from(g),
            b: f64::from(b),
            a: 1.0,
        }
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
        let fancy_cloud = FancyCloudPipeline::new(device, color_format, &camera_layout);
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

        // Built once, kept unrotated, rotated per frame in `render`.
        let star_base = build_star_field(STAR_FIELD_SEED);
        let star_quad_count = star_base.len() as u32;
        let star_vbuf = vertex_buffer(
            device,
            "lodestone-sky-star-vbuf",
            (star_base.len() * 4 * std::mem::size_of::<StarVertex>()) as u64,
        );
        let star_ibuf = quad_index_buffer(device, "lodestone-sky-star-ibuf", star_quad_count);

        let cloud_vbuf = vertex_buffer(
            device,
            "lodestone-sky-cloud-vbuf",
            (4 * std::mem::size_of::<CloudVertex>()) as u64,
        );
        let cloud_ibuf = quad_index_buffer(device, "lodestone-sky-cloud-ibuf", 1);

        // Voxelized once here (the texture is static for the session); the
        // face *list* is walked fresh every frame in `render` — see
        // `cloud_cells`'s field doc.
        let cloud_cells = CloudCells::from_rgba(cloud_image.width, cloud_image.height, &cloud_image.rgba);
        let fancy_cloud_max_faces = cloud_fancy_max_faces(CLOUD_FANCY_RADIUS_CELLS);
        let fancy_cloud_vbuf = vertex_buffer(
            device,
            "lodestone-sky-fancy-cloud-vbuf",
            (fancy_cloud_max_faces as u64) * 4 * std::mem::size_of::<SkyVertex>() as u64,
        );
        let fancy_cloud_ibuf = quad_index_buffer(device, "lodestone-sky-fancy-cloud-ibuf", fancy_cloud_max_faces);

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
            fancy_cloud,
            sunrise,
            celestial_bind_group,
            cloud_bind_group,
            sun_uv,
            moon_uv,
            cloud_size: (cloud_image.width, cloud_image.height),
            cloud_cells,
            disc_vbuf,
            disc_ibuf,
            disc_index_count,
            celestial_vbuf,
            celestial_ibuf,
            star_vbuf,
            star_ibuf,
            star_base,
            cloud_vbuf,
            cloud_ibuf,
            fancy_cloud_vbuf,
            fancy_cloud_ibuf,
            fancy_cloud_max_faces,
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
    ///
    /// # `clear` is not a decoration — it is what the world looks like below the
    /// horizon
    ///
    /// This pass clears the colour target itself and then draws a *finite*
    /// overhead disc, so every pixel the disc, the celestial quads and the
    /// clouds do not cover keeps `clear`: the whole of the frame below the
    /// horizon line, plus the thin `atan(16 / SKY_DISC_RADIUS)` ≈ 1.79° band
    /// just above it where a ray leaves the disc's rim rather than hitting it.
    /// Terrain draws over that afterwards, and whatever terrain does not reach
    /// **is** `clear` on screen.
    ///
    /// Vanilla does exactly this and it is easy to miss, because vanilla's clear
    /// does not live in `SkyRenderer` at all:
    /// `LevelRenderer.java:195-204` clears the main target to `fogColor` in its
    /// own `"clear"` pass, and every `SkyRenderer` render pass then passes
    /// `Optional.empty()` for the clear value. So the below-horizon void is the
    /// **fog colour** — which is also, by construction, the colour the disc's
    /// own gradient reaches at `sky_fog_end`, so the seam is invisible.
    ///
    /// Pass [`SkyFrame::clear_color`] for that. It was `wgpu::Color::BLACK`
    /// until this became a parameter, which is why the reported "the skybox ends
    /// too early and the bottom half is always black" was a hard *pure black*
    /// band rather than a wrong-shade one: black is not a plausible near-miss
    /// for any sky colour, it is the absence of one.
    ///
    /// It is a required parameter rather than something this method derives so
    /// that the GPU gates in `tests/sky_pipeline_gpu.rs` can keep clearing to
    /// black — their whole measure is "did anything paint here", which a
    /// sky-coloured clear satisfies for free.
    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color_view: &wgpu::TextureView,
        camera: &Camera,
        frame: &SkyFrame,
        clear: wgpu::Color,
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
        //
        // `fog_end` is the same on all ten vertices; the fragment stage divides
        // by it, so a shortened render distance saturates the outer disc to the
        // fog colour rather than moving any geometry (issue #399).
        let disc_verts: Vec<SkyDiscVertex> = sky_disc_positions(16.0)
            .into_iter()
            .map(|position| SkyDiscVertex {
                position,
                color: sky_color4,
                fog_color: fog_color4,
                fog_end: frame.sky_fog_end,
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
            let mut star_verts = Vec::with_capacity(self.star_base.len() * 4);
            for quad in &self.star_base {
                for corner in *quad {
                    let p = rotation.transform_point3(glam::Vec3::from(corner));
                    star_verts.push(StarVertex {
                        position: p.to_array(),
                        color,
                    });
                }
            }
            queue.write_buffer(&self.star_vbuf, 0, bytemuck::cast_slice(&star_verts));
        }

        // Resolved by `SkyFrame::resolve_colors` from the real `CLOUD_COLOR`
        // attribute (pure white at alpha 0.8) times the real `CLOUD_COLOR` track
        // — not from the sky colour, and with no invented darkening factor. The
        // alpha rides through to the fragment stage and needs each pipeline's
        // `CLOUD_BLEND`; an opaque pipeline would discard it silently. Shared by
        // both cloud modes, exactly as vanilla shares one `CloudColor` uniform
        // between `FLAT_CLOUDS` and `CLOUDS`.
        let cloud_tint = cloud_color;

        // FAST/FANCY selection (issue #403) — vanilla's own choice, minus
        // `OFF` (see `CloudStatus`'s doc). Only the selected mode's vertex
        // buffer is written; the other pipeline is simply not bound below.
        let draw_fast_clouds = frame.cloud_status == crate::sky::CloudStatus::Fast;
        let fancy_face_count = if draw_fast_clouds {
            let (cloud_pos, cloud_uv) = cloud_plane_geometry(
                camera.position.to_array(),
                time_of_day,
                self.cloud_size.0,
                self.cloud_size.1,
                CLOUD_PLANE_HALF_EXTENT,
            );
            let cloud_verts: Vec<CloudVertex> = (0..4)
                .map(|i| CloudVertex {
                    position: cloud_pos[i],
                    uv: cloud_uv[i],
                    color: cloud_tint,
                })
                .collect();
            queue.write_buffer(&self.cloud_vbuf, 0, bytemuck::cast_slice(&cloud_verts));
            0
        } else {
            let verts = fancy_cloud_geometry(&self.cloud_cells, camera.position.to_array(), time_of_day, cloud_tint);
            debug_assert!(
                verts.len() as u32 <= self.fancy_cloud_max_faces * 4,
                "fancy_cloud_geometry produced {} verts, over the {}-face buffer capacity — \
                 CLOUD_FANCY_RADIUS_CELLS and cloud_fancy_max_faces have drifted apart",
                verts.len(),
                self.fancy_cloud_max_faces
            );
            let face_count = (verts.len() / 4).min(self.fancy_cloud_max_faces as usize) as u32;
            if face_count > 0 {
                let gpu_verts: Vec<SkyVertex> = verts[..(face_count as usize * 4)]
                    .iter()
                    .map(|(position, color)| SkyVertex {
                        position: *position,
                        color: *color,
                    })
                    .collect();
                queue.write_buffer(&self.fancy_cloud_vbuf, 0, bytemuck::cast_slice(&gpu_verts));
            }
            face_count
        };

        let _ = device; // reserved for a future resize/rebuild path

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lodestone-sky-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // See `render`'s doc: this is the below-horizon void, not a
                    // scratch value. Vanilla's equivalent is
                    // `LevelRenderer`'s `"clear"` pass at the fog colour.
                    load: wgpu::LoadOp::Clear(clear),
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
            pass.draw_indexed(0..(self.star_base.len() as u32) * 6, 0, 0..1);
        }

        pass.set_pipeline(&self.celestial.pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(1, &self.celestial_bind_group, &[]);
        pass.set_vertex_buffer(0, self.celestial_vbuf.slice(..));
        pass.set_index_buffer(self.celestial_ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..12, 0, 0..1);

        if draw_fast_clouds {
            pass.set_pipeline(&self.cloud.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.cloud_bind_group, &[]);
            pass.set_vertex_buffer(0, self.cloud_vbuf.slice(..));
            pass.set_index_buffer(self.cloud_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..6, 0, 0..1);
        } else if fancy_face_count > 0 {
            pass.set_pipeline(&self.fancy_cloud.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_vertex_buffer(0, self.fancy_cloud_vbuf.slice(..));
            pass.set_index_buffer(self.fancy_cloud_ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..fancy_face_count * 6, 0, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #399's anti-island check, and the only one in the tree that needs no
    /// GPU adapter: the render distance can only reach the disc if the *shader*
    /// takes it as a vertex input. A `SkyFrame` field, a builder and a vertex
    /// struct field can all be correct while `sky_disc.wgsl` still divides by its
    /// old `const`, and every one of those would compile.
    #[test]
    fn the_disc_shader_takes_the_fog_end_as_a_vertex_input() {
        assert!(
            SKY_DISC_WGSL.contains("@location(3) fog_end: f32"),
            "sky_disc.wgsl no longer declares the fog end as vertex attribute 3, \
             so `SkyFrame::sky_fog_end` reaches no pixel"
        );
        assert!(
            SKY_DISC_WGSL.contains("in.fog_end"),
            "sky_disc.wgsl declares the fog-end attribute but its fragment stage \
             does not read it — an attribute the ramp ignores"
        );
        // Name-independent: any reintroduced 512-block literal, under any
        // identifier, would shadow the attribute and make the render distance
        // inert while every Rust-side test above still passed. The shader's own
        // comments say "512 blocks" and "512-block rim", never "512.0", so this
        // needle matches only a float literal.
        assert!(
            !SKY_DISC_WGSL.contains("512.0"),
            "sky_disc.wgsl has regained a 512.0 literal — the hardcoded fog end \
             that #399 replaced. The value must come from the vertex attribute"
        );
    }

    /// [`DISC_ATTRS`] must describe the **whole** vertex, because the stride
    /// passed to `vertex_layout` is `size_of::<SkyDiscVertex>()` while the
    /// attributes are written by hand: a field added to the struct without a
    /// matching attribute compiles, uploads, and is simply never read by the
    /// shader — the exact shape of an island. This is what `Pod` does not check.
    #[test]
    fn the_disc_vertex_layout_covers_the_whole_vertex() {
        // position (12) + color (16) + fog_color (16) + fog_end (4).
        assert_eq!(std::mem::size_of::<SkyDiscVertex>(), 48);
        assert_eq!(std::mem::align_of::<SkyDiscVertex>(), 4);

        let last = DISC_ATTRS
            .last()
            .expect("the disc vertex has attributes at all");
        assert_eq!(
            last.offset + last.format.size(),
            std::mem::size_of::<SkyDiscVertex>() as u64,
            "DISC_ATTRS stops at byte {} but the vertex is {} bytes: the trailing \
             field is uploaded and never read",
            last.offset + last.format.size(),
            std::mem::size_of::<SkyDiscVertex>()
        );
        // And the locations must be dense from 0, since the shader names them.
        for (i, attr) in DISC_ATTRS.iter().enumerate() {
            assert_eq!(attr.shader_location, i as u32);
        }
    }

    /// `new` must stay at the attribute's registered default so that the
    /// pre-#399 behaviour is exactly recoverable, and `with_render_distance` must
    /// be the thing that changes it.
    #[test]
    fn sky_frame_carries_a_render_distance_derived_fog_end() {
        let default = SkyFrame::new(6_000, [0.2, 0.4, 0.8]);
        assert_eq!(default.sky_fog_end, crate::sky::SKY_FOG_END_DISTANCE);

        // Hand-derived from `AtmosphericFogEnvironment.java:73`, not from the
        // helper: min(8 * 16, 512) and min(32 * 16, 512).
        assert_eq!(default.with_render_distance(8).sky_fog_end, 128.0);
        assert_eq!(default.with_render_distance(32).sky_fog_end, 512.0);
        // The builders must not disturb anything else on the frame.
        let tuned = SkyFrame::new(6_000, [0.2, 0.4, 0.8])
            .with_fog_color([0.7, 0.6, 0.45])
            .with_render_distance(8);
        assert_eq!(tuned.day_fog_color, [0.7, 0.6, 0.45]);
        assert_eq!(tuned.day_sky_color, [0.2, 0.4, 0.8]);
        assert_eq!(tuned.time_of_day, 6_000);
        // And the blocks-space builder clamps rather than trusting its caller.
        assert_eq!(default.with_sky_fog_end(4_096.0).sky_fog_end, 512.0);
        assert_eq!(default.with_sky_fog_end(128.0).sky_fog_end, 128.0);
    }

    /// Clouds are **white**, not a shade of the sky.
    ///
    /// The reported "clouds are blue-grey" had a single cause with two halves:
    /// `resolve_colors` passed `day_sky_color` as the base for the `CLOUD_COLOR`
    /// track, and then scaled the result by an invented `0.9`. Vanilla's base is
    /// the `CLOUD_COLOR` attribute, `ARGB.white(0.8F)` — RGB `0xFFFFFF`, alpha
    /// `0.8` (`DimensionTypes.java:37`, `ARGB.java:188`).
    ///
    /// The discriminator has to be *chromatic*, not brightness: the old
    /// expression was `sky × 0.9`, so any "the clouds are bright at noon" check
    /// passes under both. What only white satisfies is **R == G == B**, and the
    /// old value could not: `SKY_COLOR`'s blue is 3.4x its red.
    #[test]
    fn noon_clouds_are_white_at_vanillas_alpha() {
        // A deliberately saturated day sky, so "clouds ignore the sky colour" is
        // a strong statement rather than a near-miss.
        let sky_blue = [0.242_867_f32, 0.462_361, 0.827_571];
        let frame = SkyFrame::new(6_000, sky_blue);
        let (_, _, cloud, _) = frame.resolve_colors(64.0);

        // Noon: the CLOUD_COLOR track's keyframes at 133 and 11_867 are both
        // #FFFFFF, so the multiply is the identity and the base shows through.
        assert!(
            (cloud[0] - 1.0).abs() < 1e-4
                && (cloud[1] - 1.0).abs() < 1e-4
                && (cloud[2] - 1.0).abs() < 1e-4,
            "a noon cloud must be pure white (the CLOUD_COLOR attribute), got {cloud:?}"
        );
        assert!(
            (cloud[3] - 0.8).abs() < 1e-6,
            "the cloud alpha must be ARGB.white(0.8)'s 0.8, got {}",
            cloud[3]
        );
        // And the pre-fix value must be excluded, chromatically: `sky * 0.9` has
        // blue 3.4x red, white has a ratio of exactly 1.
        let bug = sky_blue.map(|c| c * 0.9);
        assert!(
            (cloud[2] / cloud[0] - 1.0).abs() < 1e-3,
            "clouds must be achromatic; blue/red is {:.3}",
            cloud[2] / cloud[0]
        );
        assert!(
            (bug[2] / bug[0]) > 3.0,
            "control: the pre-fix expression really was strongly blue (ratio {:.3}), \
             so the assertion above can tell the two apart",
            bug[2] / bug[0]
        );

        // Night keeps its own non-black track (#191926) rather than following the
        // sky to true black, and the alpha does not move: every CLOUD_COLOR
        // keyframe has alpha 0xff, so the per-tick multiply leaves 0.8 alone.
        let night = SkyFrame::new(18_000, sky_blue).resolve_colors(64.0).2;
        assert!(
            night[0] > 0.0 && night[2] > night[0],
            "night clouds must stay visible and faintly blue (#191926): {night:?}"
        );
        assert!((night[3] - 0.8).abs() < 1e-6, "night cloud alpha: {}", night[3]);
        assert!(
            night[2] < cloud[2] * 0.1,
            "night clouds must be far darker than noon's: {night:?} vs {cloud:?}"
        );
    }

    /// The clear colour and the disc's outermost fragment must be the *same*
    /// colour, or the horizon bands — which is the whole reason
    /// [`SkyFrame::clear_color`] reads the resolved fog colour rather than
    /// `day_fog_color` or a second constant.
    ///
    /// `sky_disc.wgsl`'s fragment stage is
    /// `mix(color.rgb, fog_color.rgb, clamp(length/fog_end, 0, 1) * fog_color.a)`
    /// with `fog_color.a == 1.0`, so at `length >= fog_end` it is `mix(sky, fog,
    /// 1.0)`. `crate::fog::apply_fog` is that expression in Rust. Asserting the
    /// identity here rather than describing it is what would catch a future
    /// "clear from the day colour" or "clear from `sky_color`" regression, both
    /// of which compile and neither of which any GPU gate looks at (they all
    /// clear to black deliberately).
    #[test]
    fn the_clear_colour_is_the_discs_own_rim() {
        // Two clocks and two eye heights, so neither the FOG_COLOR track nor
        // void fog can be the thing that happens to make this hold.
        for time in [1_000_i64, 6_000, 13_000, 18_000, 23_500] {
            for eye_y in [-60.0_f32, -40.0, 64.0] {
                let frame = SkyFrame::new(time, [0.25, 0.46, 0.83])
                    .with_fog_color([0.31, 0.53, 0.86])
                    .with_render_distance(8)
                    .with_void_fog(crate::fog::VoidFog::OVERWORLD);
                let (sky, fog, _, _) = frame.resolve_colors(eye_y);
                let rim = crate::fog::apply_fog(sky, fog, 1.0);
                let clear = frame.clear_color(eye_y);
                // `apply_fog` is `a + (b - a) * t`, so `t == 1.0` is `b` only up
                // to one rounding step — hence a tolerance rather than `==`.
                for c in 0..3 {
                    assert!(
                        (clear[c] - rim[c]).abs() < 1e-6,
                        "t={time} eye_y={eye_y} channel {c}: the clear colour must equal \
                         the disc's rim, or the horizon bands — clear {clear:?} rim {rim:?}"
                    );
                }
            }
        }

        // And the clear must actually *track* the clock — a clear taken from
        // `day_fog_color` would be identical at noon and midnight, which is the
        // specific mistake this guards.
        let day = SkyFrame::new(6_000, [0.25, 0.46, 0.83]).with_fog_color([0.31, 0.53, 0.86]);
        let night = SkyFrame::new(18_000, [0.25, 0.46, 0.83]).with_fog_color([0.31, 0.53, 0.86]);
        assert!(
            night.clear_color(64.0)[2] < day.clear_color(64.0)[2] * 0.25,
            "the night clear must be far darker than the day clear: day {:?} night {:?}",
            day.clear_color(64.0),
            night.clear_color(64.0)
        );
        // But never black: `FOG_COLOR_TRACK` bottoms out at #161616, which is
        // why the night horizon reads faintly blue-grey. A black clear here is
        // the reported defect.
        assert!(
            night.clear_color(64.0)[2] > 0.0,
            "the night clear must not be black: {:?}",
            night.clear_color(64.0)
        );
    }
}
