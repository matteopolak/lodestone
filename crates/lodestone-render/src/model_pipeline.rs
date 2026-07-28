//! The model render pass: a depth-tested pipeline that consumes wide
//! [`ModelVertex`](crate::models::ModelVertex) geometry produced by
//! [`mesh_models`](crate::models::mesh_models) and samples the stitched block
//! atlas with the *baked* UVs each quad already carries.
//!
//! This is the counterpart to [`BlockPipeline`](crate::block::BlockPipeline).
//! The packed path exists for the full-cube majority; this path renders
//! everything a baked model can be — stairs, fences, cross-plants, fluids and
//! tinted cubes — because in the real asset pipeline *every* block is a model
//! (see the D1 note in [`crate::models`]). It is deliberately simpler than the
//! packed pipeline in one respect: [`BakedQuad`](lodestone_assets::BakedQuad)
//! UVs are already absolute normalised atlas coordinates, so there is no
//! sprite-rect indirection buffer — the vertex UV maps straight onto the atlas.
//!
//! # Tint
//!
//! A baked quad with a `tint_index` carries a **palette index** rather than the
//! raw model index: [`BlockModels::build`](crate::BlockModels::build) resolves
//! each tinted quad's real default (plains) colour — grass, foliage, dry-foliage,
//! water, the fixed constants, redstone levels — and interns it into a small
//! palette ([`BlockModels::tint_palette`](crate::BlockModels::tint_palette)). The
//! shader multiplies the sampled texel by `palette[tint]` (slot 255 is white, so
//! untinted quads pass through). This replaces the earlier single hardcoded
//! green, which collapsed every tinted source to one colour — the defect that
//! made leaves render grass-green and grass side-overlays over-saturated.
//! Per-*biome* tint (sampling the live biome instead of plains) is still a
//! follow-up; the palette is the plains default the colormaps resolve to.
//!
//! # Render layer
//!
//! The pipeline is built per [`RenderLayer`](crate::translucency::RenderLayer)
//! just like the packed one. A fragment whose sampled alpha is below a cutout
//! threshold is discarded, so cutout sprites (cross-plants, leaves) render
//! correctly on the opaque pass without a separate material.

use wgpu::util::DeviceExt;

use crate::block::{CameraUniform, DEPTH_FORMAT};
use crate::models::{ModelMesh, ModelVertex};
use crate::texture::GpuAtlas;
use crate::translucency::RenderLayer;

/// GPU-resident wide-model geometry: a vertex buffer, an index buffer and the
/// index count.
#[derive(Debug)]
pub struct GpuModelMesh {
    /// Vertex buffer of [`ModelVertex`].
    pub vertices: wgpu::Buffer,
    /// `u32` index buffer.
    pub indices: wgpu::Buffer,
    /// Number of indices to draw.
    pub index_count: u32,
}

impl GpuModelMesh {
    /// Upload a [`ModelMesh`], or `None` if it is empty (nothing to draw).
    #[must_use]
    pub fn upload(device: &wgpu::Device, mesh: &ModelMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-model-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-model-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuModelMesh {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
        })
    }
}

/// A depth-tested pipeline for wide baked-model geometry.
#[derive(Debug)]
pub struct ModelPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the camera uniform (group 0).
    pub camera_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the atlas texture + sampler (group 1).
    pub atlas_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the tint palette uniform (group 2). Present only on
    /// the model pipeline; the fluid pipeline carries its own tint and has none.
    pub palette_layout: Option<wgpu::BindGroupLayout>,
}

impl ModelPipeline {
    /// Build the opaque (`Solid`) model pipeline targeting `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::for_layer(device, color_format, RenderLayer::Solid)
    }

    /// Build the pipeline for a specific [`RenderLayer`]. `Solid`/`Cutout` use
    /// an opaque target with depth writes and back-face culling; `Translucent`
    /// enables alpha blending, disables depth writes and back-face culling, and
    /// expects the caller to have sorted quads back-to-front.
    #[must_use]
    pub fn for_layer(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        layer: RenderLayer,
    ) -> Self {
        let translucent = layer == RenderLayer::Translucent;
        Self::build(device, color_format, MODEL_WGSL, translucent, true)
    }

    /// Build the translucent **fluid** pipeline: like a `Translucent` model
    /// pipeline (alpha blending, no depth writes, no back-face culling) but with
    /// a shader that does **not** cutout-discard (water is a smooth alpha, not a
    /// mask) and tints `tint_index` quads with the water colour instead of
    /// foliage green. Drawn after opaque terrain so the sea floor already in the
    /// depth buffer shows through.
    #[must_use]
    pub fn for_fluid(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::build(device, color_format, FLUID_WGSL, true, false)
    }

    fn build(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        shader_src: &str,
        translucent: bool,
        with_palette: bool,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-model-shader"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-model-camera-bgl"),
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

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-model-atlas-bgl"),
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
        });

        let palette_layout = with_palette.then(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("lodestone-model-palette-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            })
        });

        let mut bind_group_layouts = vec![Some(&camera_layout), Some(&atlas_layout)];
        if let Some(palette_layout) = &palette_layout {
            bind_group_layouts.push(Some(palette_layout));
        }
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-model-layout"),
            bind_group_layouts: &bind_group_layouts,
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-model-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(ModelVertex::vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: if translucent {
                        Some(wgpu::BlendState::ALPHA_BLENDING)
                    } else {
                        None
                    },
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: if translucent {
                    None
                } else {
                    Some(wgpu::Face::Back)
                },
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(!translucent),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        ModelPipeline {
            pipeline,
            camera_layout,
            atlas_layout,
            palette_layout,
        }
    }

    /// Create the camera bind group from an existing uniform buffer.
    #[must_use]
    pub fn camera_bind_group(
        &self,
        device: &wgpu::Device,
        camera_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-model-camera-bg"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        })
    }

    /// Create the atlas bind group from a [`GpuAtlas`] (texture + sampler only).
    #[must_use]
    pub fn atlas_bind_group(&self, device: &wgpu::Device, atlas: &GpuAtlas) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-model-atlas-bg"),
            layout: &self.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&atlas.sampler),
                },
            ],
        })
    }

    /// Create the tint-palette bind group (group 2) from a palette uniform
    /// buffer built with [`model_palette_buffer`].
    ///
    /// # Panics
    ///
    /// Panics if called on a pipeline built without a palette (the fluid
    /// pipeline); only the model pipeline carries group 2.
    #[must_use]
    pub fn palette_bind_group(
        &self,
        device: &wgpu::Device,
        palette_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let layout = self
            .palette_layout
            .as_ref()
            .expect("palette_bind_group requires a pipeline built with a palette");
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-model-palette-bg"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: palette_buffer.as_entire_binding(),
            }],
        })
    }
}

/// Build the tint-palette uniform buffer the model shader looks up per tinted
/// quad. `palette` is [`BlockModels::tint_palette`](crate::BlockModels::tint_palette)
/// — one straight RGBA multiplier per palette index.
#[must_use]
pub fn model_palette_buffer(device: &wgpu::Device, palette: &[[f32; 4]]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-model-palette-uniform"),
        contents: bytemuck::cast_slice(palette),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

/// Reuse the packed pass's camera uniform buffer builder for the model pass; the
/// [`CameraUniform`] layout is identical.
#[must_use]
pub fn model_camera_buffer(device: &wgpu::Device, uniform: CameraUniform) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-model-camera-uniform"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

const MODEL_WGSL: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
    section_origin: vec4<f32>,
};

// The default (plains) tint palette. A quad's tint byte indexes this; slot 255
// is white (untinted). Replaces the single hardcoded green so grass, foliage and
// every other tinted source render their own colour.
struct Palette {
    colors: array<vec4<f32>, 256>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;
@group(2) @binding(0) var<uniform> palette: Palette;

// sRGB transfer functions (component-wise). The atlas is an _srgb texture, so
// `textureSample` returns linear-light texels; the tint palette holds straight
// sRGB bytes. Multiplying a linear texel by an sRGB tint and then re-encoding on
// the sRGB surface gamma-compresses the tint's green/red ratio (grass 1.30 ->
// ~1.13, measurably greyer than vanilla). Vanilla applies the biome tint in
// gamma space, so we convert the texel to sRGB, tint there, then convert back.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) shade: f32,
    @location(2) @interpolate(flat) tint_idx: u32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) ao: f32,
    @location(3) packed: vec4<u32>,
) -> VsOut {
    let light_byte = packed.x;
    let sky = f32((light_byte >> 4u) & 15u) / 15.0;
    let block = f32(light_byte & 15u) / 15.0;

    let world = position + camera.section_origin.xyz;
    // Lift a dark floor so unlit faces read dim rather than pure black.
    let light_term = 0.2 + 0.8 * max(sky, block);

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.uv = uv;
    out.shade = ao * light_term;
    out.tint_idx = packed.y;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex = textureSample(atlas_tex, atlas_smp, in.uv);
    // Cutout: drop near-transparent texels (cross-plants, leaves) so they render
    // correctly on the opaque pass.
    if (tex.a < 0.5) {
        discard;
    }
    // Per-quad tint: the palette slot resolves grass/foliage/etc. to their real
    // default colour; the untinted slot (255) leaves the texel untouched. The
    // tint is applied in gamma space (see the transfer functions above) so its
    // green/red ratio survives the sRGB surface encode and matches vanilla.
    var rgb = tex.rgb;
    if (in.tint_idx != 255u) {
        let tint_col = palette.colors[in.tint_idx].rgb;
        rgb = srgb_to_linear(linear_to_srgb(rgb) * tint_col);
    }
    return vec4<f32>(rgb * in.shade, tex.a);
}
";

// The fluid (water) shader. Unlike the model shader it does **not** discard on
// low alpha — water is a smooth translucent surface, not a cutout mask — and it
// tints `tint_index` quads with the default water colour (#3F76E4) rather than
// foliage green. Water's greyscale texture becomes blue here.
const FLUID_WGSL: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
    section_origin: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) shade: f32,
    @location(2) tinted: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) ao: f32,
    @location(3) packed: vec4<u32>,
) -> VsOut {
    let light_byte = packed.x;
    let sky = f32((light_byte >> 4u) & 15u) / 15.0;
    let block = f32(light_byte & 15u) / 15.0;
    let tint_idx = packed.y;

    let world = position + camera.section_origin.xyz;
    let light_term = 0.2 + 0.8 * max(sky, block);

    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    out.uv = uv;
    out.shade = ao * light_term;
    out.tinted = select(0.0, 1.0, tint_idx != 255u);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex = textureSample(atlas_tex, atlas_smp, in.uv);
    // Default water colour (#3F76E4); untinted quads keep their own colour.
    let water = vec3<f32>(0.247, 0.463, 0.894);
    let tint_col = mix(vec3<f32>(1.0, 1.0, 1.0), water, in.tinted);
    return vec4<f32>(tex.rgb * tint_col * in.shade, tex.a);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_model_mesh_uploads_to_none() {
        // The empty check short-circuits before any GPU call, so this needs no
        // device.
        let mesh = ModelMesh::default();
        assert!(mesh.indices.is_empty());
    }

    #[test]
    fn model_vertex_layout_is_28_bytes_over_four_attributes() {
        let layout = ModelVertex::vertex_layout();
        assert_eq!(layout.array_stride, 28);
        assert_eq!(layout.attributes.len(), 4);
        // Last attribute (packed light/tint tail) starts at offset 24.
        assert_eq!(layout.attributes[3].offset, 24);
    }
}
