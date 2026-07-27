//! The entity render pass: an **instanced**, depth-tested pipeline that draws a
//! baked [`EntityMesh`] once per model type and reads
//! each visible entity's world transform from a per-instance matrix.
//!
//! This is the entity counterpart to [`ModelPipeline`](crate::model_pipeline).
//! It reuses the wide [`ModelVertex`] layout for the
//! mesh — so a baked mob shares vertex plumbing with baked blocks — but differs
//! in the one way entities require: the vertex position is transformed by a
//! per-instance `mat4x4` supplied through a second, `Instance`-step vertex
//! buffer. That is what makes a mob farm of hundreds of the same model a single
//! instanced draw with one small matrix per mob, rather than hundreds of
//! meshes.
//!
//! # Bindings and buffers
//!
//! * **Group 0**: the camera uniform ([`CameraUniform`],
//!   reused; only `view_proj` is read — `section_origin` is left zero because an
//!   entity's world position lives in its instance matrix, not a section origin).
//! * **Group 1**: the entity's texture sheet + sampler.
//! * **Vertex buffer 0**: [`ModelVertex`] (locations 0–3; the shader reads
//!   position and UV).
//! * **Vertex buffer 1**: [`EntityInstanceRaw`] (locations 4–7 = the four columns
//!   of the model matrix), stepped per instance.
//!
//! # Shading without a per-vertex normal
//!
//! [`ModelVertex`] carries no normal, so the fragment shader reconstructs a face
//! normal from screen-space derivatives of the interpolated world position
//! (`cross(dpdx, dpdy)`) and applies a cheap directional term. Using the
//! magnitude of the light dot means the shade is correct whether a face is
//! front- or back-facing, which pairs with the double-sided raster state below:
//! entity meshes are drawn without back-face culling for now (robust visibility
//! while per-model winding parity is still being pixel-verified), so both sides
//! shade consistently rather than one going black.

use wgpu::util::DeviceExt;

use crate::block::{CameraUniform, DEPTH_FORMAT};
use crate::entity::EntityMesh;
use crate::models::ModelVertex;

/// A per-instance entity transform for the instance vertex buffer: a column-major
/// `mat4x4<f32>` laid out as four `vec4` attributes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityInstanceRaw {
    /// The model→world matrix, column-major (four columns of four floats).
    pub model: [[f32; 4]; 4],
}

impl EntityInstanceRaw {
    /// Pack a [`glam::Mat4`] into the instance format (column-major).
    #[must_use]
    pub fn from_mat4(m: glam::Mat4) -> Self {
        Self {
            model: m.to_cols_array_2d(),
        }
    }

    /// The instance-stepped vertex-buffer layout: four `Float32x4` columns at
    /// shader locations 4–7.
    #[must_use]
    pub const fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 4] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 0,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 16,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 32,
                shader_location: 6,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 48,
                shader_location: 7,
            },
        ];
        wgpu::VertexBufferLayout {
            array_stride: core::mem::size_of::<EntityInstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }
}

/// GPU-resident geometry for one entity model type: a vertex buffer, an index
/// buffer, and the index count. Uploaded once; every instance of the model
/// reuses it.
#[derive(Debug)]
pub struct GpuEntityModel {
    /// Vertex buffer of [`ModelVertex`].
    pub vertices: wgpu::Buffer,
    /// `u32` index buffer.
    pub indices: wgpu::Buffer,
    /// Number of indices to draw.
    pub index_count: u32,
}

impl GpuEntityModel {
    /// Upload an [`EntityMesh`], or `None` if it is empty (nothing to draw).
    #[must_use]
    pub fn upload(device: &wgpu::Device, mesh: &EntityMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuEntityModel {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
        })
    }
}

/// Build an instance buffer from a slice of model matrices, or `None` if empty.
#[must_use]
pub fn upload_instances(device: &wgpu::Device, transforms: &[glam::Mat4]) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .map(|m| EntityInstanceRaw::from_mat4(*m))
        .collect();
    Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-instances"),
            contents: bytemuck::cast_slice(&raw),
            usage: wgpu::BufferUsages::VERTEX,
        }),
    )
}

/// A depth-tested, instanced pipeline for baked entity geometry.
#[derive(Debug)]
pub struct EntityPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the camera uniform (group 0).
    pub camera_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the entity texture + sampler (group 1).
    pub texture_layout: wgpu::BindGroupLayout,
}

impl EntityPipeline {
    /// Build the entity pipeline targeting `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-entity-shader"),
            source: wgpu::ShaderSource::Wgsl(ENTITY_WGSL.into()),
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-entity-camera-bgl"),
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

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-entity-texture-bgl"),
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

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-entity-layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-entity-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[
                    Some(ModelVertex::vertex_layout()),
                    Some(EntityInstanceRaw::instance_layout()),
                ],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                // Double-sided for now: robust visibility while per-model winding
                // parity is pixel-verified. See the module docs.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        EntityPipeline {
            pipeline,
            camera_layout,
            texture_layout,
        }
    }

    /// Build the camera uniform buffer for the entity pass. `view_proj` is taken
    /// from the camera; `section_origin` is unused (zero) because an entity's
    /// world position lives in its instance matrix.
    #[must_use]
    pub fn camera_buffer(
        &self,
        device: &wgpu::Device,
        camera: &crate::camera::Camera,
    ) -> wgpu::Buffer {
        let uniform = CameraUniform::new(camera, [0.0, 0.0, 0.0]);
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-entity-camera-uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        })
    }

    /// Create the camera bind group from a uniform buffer.
    #[must_use]
    pub fn camera_bind_group(
        &self,
        device: &wgpu::Device,
        camera_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-entity-camera-bg"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        })
    }

    /// Create the texture bind group from a texture view and sampler (one
    /// entity sheet).
    #[must_use]
    pub fn texture_bind_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-entity-texture-bg"),
            layout: &self.texture_layout,
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
}

const ENTITY_WGSL: &str = r"
struct Camera {
    view_proj: mat4x4<f32>,
    section_origin: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
) -> VsOut {
    let model = mat4x4<f32>(m0, m1, m2, m3);
    let world = model * vec4<f32>(position, 1.0);
    var out: VsOut;
    out.clip = camera.view_proj * world;
    out.uv = uv;
    out.world = world.xyz;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex_col = textureSample(tex, smp, in.uv);
    // Cutout transparent texels (e.g. between legs on a sheet with padding).
    if (tex_col.a < 0.5) {
        discard;
    }
    // Reconstruct a face normal from world-position derivatives so the mob reads
    // as 3D without a per-vertex normal. abs() keeps both sides lit (double-sided).
    let n = normalize(cross(dpdx(in.world), dpdy(in.world)));
    let light_dir = normalize(vec3<f32>(0.3, 1.0, 0.55));
    let shade = 0.4 + 0.6 * clamp(abs(dot(n, light_dir)), 0.0, 1.0);
    return vec4<f32>(tex_col.rgb * shade, tex_col.a);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_raw_is_64_bytes_over_four_columns() {
        assert_eq!(core::mem::size_of::<EntityInstanceRaw>(), 64);
        let layout = EntityInstanceRaw::instance_layout();
        assert_eq!(layout.array_stride, 64);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
        assert_eq!(layout.attributes.len(), 4);
        // Instance attributes start at location 4, past ModelVertex's 0..=3.
        assert_eq!(layout.attributes[0].shader_location, 4);
        assert_eq!(layout.attributes[3].shader_location, 7);
        assert_eq!(layout.attributes[3].offset, 48);
    }

    #[test]
    fn from_mat4_is_column_major() {
        // A translation matrix: glam stores translation in the 4th column, so
        // the packed [3] row must carry it (column-major → model[3] is col 3).
        let m = glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
        let raw = EntityInstanceRaw::from_mat4(m);
        assert_eq!(raw.model[3][0], 1.0);
        assert_eq!(raw.model[3][1], 2.0);
        assert_eq!(raw.model[3][2], 3.0);
        assert_eq!(raw.model[3][3], 1.0);
    }
}
