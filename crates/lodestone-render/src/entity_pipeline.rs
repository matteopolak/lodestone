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
//! * **Group 0**: [`EntityCameraUniform`] — the camera ([`CameraUniform`];
//!   only `view_proj` is read, `section_origin` is left zero because an entity's
//!   world position lives in its instance matrix) **followed by this frame's
//!   [`FogUniform`]**. Fog is folded in here rather than given its own bind
//!   group, matching [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform):
//!   the fog block must travel with the camera anyway, and one uniform means
//!   the entity pass can never drift out of step with the terrain pass's fog.
//! * **Group 1**: the entity's texture sheet + sampler.
//! * **Vertex buffer 0**: [`ModelVertex`] (locations 0–3; the shader reads
//!   position and UV).
//! * **Vertex buffer 1**: [`EntityInstanceRaw`] (locations 4–7 = the four columns
//!   of the model matrix, location 8 = the packed light byte), stepped per
//!   instance.
//!
//! # Shading: world light per instance, direction per fragment
//!
//! A mob's brightness has two independent factors and they are applied in
//! different spaces for a reason:
//!
//! 1. **World light**, one packed sky/block byte per *instance*. Vanilla samples
//!    the lightmap once per entity at its block position, so a mob is uniformly
//!    lit by the block it stands in; this shader reproduces terrain's
//!    `light_term = 0.2 + 0.8 * max(sky, block)` from that byte. Without it a
//!    mob renders full-bright and out-shines the terrain around it by up to an
//!    order of magnitude at night — the reported "mobs are super bright, blocks
//!    are dark" defect, in which nothing was wrong with the blocks.
//! 2. **Direction.** [`ModelVertex`] carries no normal, so the fragment shader
//!    reconstructs a face normal from screen-space derivatives of the
//!    interpolated world position (`cross(dpdx, dpdy)`) and applies a cheap
//!    directional term. Using the magnitude of the light dot means the shade is
//!    correct whether a face is front- or back-facing, which pairs with the
//!    double-sided raster state below: entity meshes are drawn without back-face
//!    culling for now (robust visibility while per-model winding parity is still
//!    being pixel-verified), so both sides shade consistently rather than one
//!    going black.
//!
//! Their product is multiplied into the texel in **gamma space**, through the
//! same `srgb_to_linear(linear_to_srgb(rgb) * shade)` round-trip the model
//! shader uses. Vanilla is not colour-managed and multiplies shade into gamma
//! byte values; doing it in linear light and re-encoding pulls every factor
//! toward 1.0 (a shade of 0.6 reads as 0.79), which is the washed-out look
//! `4e8f058` removed from terrain. Entities carried the same bug afterwards.
//!
//! # Texture format is part of the brightness
//!
//! The sheet bound to group 1 must be an **`_srgb`** format, like the block
//! atlas. A vanilla PNG holds gamma-encoded bytes; binding it as plain `Unorm`
//! hands the shader `0.50` where the linear value is `0.21`, and the sRGB render
//! target then encodes it a *second* time — a measured **+48%** on every mob
//! pixel, enough on its own to make a mob brighter than the brightest sunlit
//! block face.

use wgpu::util::DeviceExt;

use crate::block::{CameraUniform, DEPTH_FORMAT};
use crate::entity::EntityMesh;
use crate::models::ModelVertex;

/// A per-instance entity record for the instance vertex buffer: a column-major
/// `mat4x4<f32>` laid out as four `vec4` attributes, plus the entity's packed
/// sky/block light byte.
///
/// Light rides the *instance* buffer, not the vertex buffer, because the vertex
/// buffer is shared by every instance of a model type — a per-vertex light byte
/// could only ever say one thing for all mobs of that kind. Vanilla's own
/// lightmap sample is per entity, so this is also the faithful granularity.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityInstanceRaw {
    /// The model→world matrix, column-major (four columns of four floats).
    pub model: [[f32; 4]; 4],
    /// Packed sky/block light, `sky << 4 | block` (`0..=15` each), widened to
    /// `u32` for the `Uint32` vertex attribute. Same encoding as
    /// [`ModelVertex::light`](crate::models::ModelVertex::light), so the entity
    /// and model shaders unpack it with identical code.
    pub light: u32,
}

impl EntityInstanceRaw {
    /// Pack a [`glam::Mat4`] into the instance format (column-major), lit
    /// full-bright. Kept for callers with no world to sample.
    #[must_use]
    pub fn from_mat4(m: glam::Mat4) -> Self {
        Self::new(m, u32::from(crate::entity::ENTITY_FULLBRIGHT))
    }

    /// Pack a transform and a packed sky/block light byte into the instance
    /// format (column-major).
    #[must_use]
    pub fn new(m: glam::Mat4, light: u32) -> Self {
        Self {
            model: m.to_cols_array_2d(),
            light,
        }
    }

    /// The instance-stepped vertex-buffer layout: four `Float32x4` columns at
    /// shader locations 4–7, then the packed light `Uint32` at location 8.
    #[must_use]
    pub const fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 5] = [
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
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 64,
                shader_location: 8,
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
    /// Number of indices to draw (all parts).
    pub index_count: u32,
    /// One index sub-range per skeleton part, in mesh part order. Drawing part
    /// `p` instanced over that part's matrices is what animates a limb.
    pub parts: Vec<crate::entity::PartRange>,
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
            parts: mesh.parts.clone(),
        })
    }
}

/// Build an instance buffer from a slice of model matrices and the matching
/// per-instance packed light bytes, or `None` if empty.
///
/// `lights` is indexed in lockstep with `transforms`
/// ([`EntityBatch::lights`](crate::entity::EntityBatch::lights) alongside any of
/// that batch's per-part matrix vectors). A short or missing `lights` entry
/// falls back to [`ENTITY_FULLBRIGHT`](crate::entity::ENTITY_FULLBRIGHT) rather
/// than panicking or rendering black: a light plumbing mistake should look like
/// the old behaviour, not like a crash mid-frame.
#[must_use]
pub fn upload_instances(
    device: &wgpu::Device,
    transforms: &[glam::Mat4],
    lights: &[u32],
) -> Option<wgpu::Buffer> {
    if transforms.is_empty() {
        return None;
    }
    let fallback = u32::from(crate::entity::ENTITY_FULLBRIGHT);
    let raw: Vec<EntityInstanceRaw> = transforms
        .iter()
        .enumerate()
        .map(|(i, m)| EntityInstanceRaw::new(*m, lights.get(i).copied().unwrap_or(fallback)))
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
                // Vertex reads the view-projection; fragment reads the folded
                // fog block (eye, colour, range), so both stages bind it.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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

    /// Build the group-0 uniform buffer for the entity pass with fog
    /// **disabled**. `view_proj` is taken from the camera; `section_origin` is
    /// unused (zero) because an entity's world position lives in its instance
    /// matrix.
    ///
    /// The buffer is sized for the whole [`EntityCameraUniform`], so a caller
    /// that later wants fog can overwrite it in place with
    /// [`queue.write_buffer`](wgpu::Queue::write_buffer) — see
    /// [`camera_buffer_with_fog`](Self::camera_buffer_with_fog).
    #[must_use]
    pub fn camera_buffer(
        &self,
        device: &wgpu::Device,
        camera: &crate::camera::Camera,
    ) -> wgpu::Buffer {
        self.camera_buffer_with_fog(device, camera, crate::fog::FogUniform::disabled())
    }

    /// Build the group-0 uniform buffer for the entity pass with an explicit fog
    /// block, so mobs fade into the distance (or into water fog) on exactly the
    /// same curve as the terrain behind them.
    #[must_use]
    pub fn camera_buffer_with_fog(
        &self,
        device: &wgpu::Device,
        camera: &crate::camera::Camera,
        fog: crate::fog::FogUniform,
    ) -> wgpu::Buffer {
        entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform::new(camera, [0.0, 0.0, 0.0]),
                fog,
            },
        )
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

/// The group-0 uniform for the entity pipeline: the [`CameraUniform`] followed
/// by this frame's [`FogUniform`](crate::fog::FogUniform).
///
/// Byte-compatible with
/// [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform) on purpose
/// — same layout, same shader-side `Camera` struct, same `fog_amount` — so a
/// mob and the block behind it can never be fogged by different math. Rewrite
/// the whole struct each frame via [`queue.write_buffer`](wgpu::Queue::write_buffer).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EntityCameraUniform {
    /// View-projection (and an unused zero section origin).
    pub camera: CameraUniform,
    /// Distance fog for this frame (eye position, colour, start/end).
    pub fog: crate::fog::FogUniform,
}

/// Create the entity pass's group-0 uniform buffer from a full
/// [`EntityCameraUniform`]. For callers holding a [`Camera`](crate::camera::Camera),
/// [`EntityPipeline::camera_buffer_with_fog`] is the convenient wrapper.
#[must_use]
pub fn entity_camera_buffer(
    device: &wgpu::Device,
    uniform: EntityCameraUniform,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-entity-camera-uniform"),
        contents: bytemuck::bytes_of(&uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

const ENTITY_WGSL: &str = r"
// Camera plus this frame's distance fog, folded into one group-0 uniform — the
// same layout the model/fluid shaders use, so entities and terrain fog
// identically. `fog_eye.xyz` is the camera world position; `fog_color_start.rgb`
// is the fog colour and `.w` where fog begins; `fog_end_enabled.x` is where fog
// is full and `.y` is 0/1.
struct Camera {
    view_proj: mat4x4<f32>,
    section_origin: vec4<f32>,
    fog_eye: vec4<f32>,
    fog_color_start: vec4<f32>,
    fog_end_enabled: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

// Identical to the model shader's `fog_amount` and to `crate::fog::fog_factor`.
fn fog_amount(dist: f32) -> f32 {
    let start = camera.fog_color_start.w;
    let end = camera.fog_end_enabled.x;
    let enabled = camera.fog_end_enabled.y;
    if (end <= start) {
        return 0.0;
    }
    return clamp((dist - start) / (end - start), 0.0, 1.0) * enabled;
}

// sRGB transfer functions, as in the model shader: vanilla is not colour
// managed and multiplies shade into gamma byte values, so the shade multiply
// happens between these two, not in linear light.
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
    @location(1) world: vec3<f32>,
    // Flat: world light is one lightmap sample for the whole entity (vanilla's
    // granularity), so interpolating it across a mob would be meaningless.
    @location(2) @interpolate(flat) light_term: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
    @location(8) light: u32,
) -> VsOut {
    let model = mat4x4<f32>(m0, m1, m2, m3);
    let world = model * vec4<f32>(position, 1.0);
    // Byte-for-byte the model shader's light term, including the 0.2 floor that
    // keeps unlit surfaces dim rather than pure black. Any drift between the two
    // shows up as mobs that do not belong to the scene they stand in.
    let sky = f32((light >> 4u) & 15u) / 15.0;
    let block = f32(light & 15u) / 15.0;
    var out: VsOut;
    out.clip = camera.view_proj * world;
    out.uv = uv;
    out.world = world.xyz;
    out.light_term = 0.2 + 0.8 * max(sky, block);
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
    let diffuse = 0.4 + 0.6 * clamp(abs(dot(n, light_dir)), 0.0, 1.0);
    // Direction and world light are one shade, multiplied in gamma space through
    // a single transfer round-trip (one round-trip, not one per factor, so there
    // is less rounding) — exactly the model shader's treatment of `ao * light`.
    let lit = srgb_to_linear(linear_to_srgb(tex_col.rgb) * diffuse * in.light_term);
    // Fade toward the fog colour by view distance, on the same curve as terrain,
    // so a mob at the render-distance edge or under water dissolves with the
    // blocks around it instead of hanging in front of them.
    let amount = fog_amount(length(in.world - camera.fog_eye.xyz));
    return vec4<f32>(mix(lit, camera.fog_color_start.rgb, amount), tex_col.a);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_raw_is_four_columns_plus_a_light_word() {
        assert_eq!(core::mem::size_of::<EntityInstanceRaw>(), 68);
        let layout = EntityInstanceRaw::instance_layout();
        assert_eq!(layout.array_stride, 68);
        assert_eq!(layout.step_mode, wgpu::VertexStepMode::Instance);
        assert_eq!(layout.attributes.len(), 5);
        // Instance attributes start at location 4, past ModelVertex's 0..=3.
        assert_eq!(layout.attributes[0].shader_location, 4);
        assert_eq!(layout.attributes[3].shader_location, 7);
        assert_eq!(layout.attributes[3].offset, 48);
        // The light word sits immediately after the matrix.
        assert_eq!(layout.attributes[4].shader_location, 8);
        assert_eq!(layout.attributes[4].offset, 64);
        assert_eq!(layout.attributes[4].format, wgpu::VertexFormat::Uint32);
    }

    /// The uniform the entity shader's `Camera` struct maps onto: 80 bytes of
    /// camera (a `mat4x4` plus a `vec4`) then 48 of fog (three `vec4`s). If this
    /// ever stops matching the model pipeline's uniform, the two passes would fog
    /// differently and a mob would visibly detach from its background.
    #[test]
    fn camera_uniform_matches_the_model_pipelines_layout() {
        assert_eq!(core::mem::size_of::<EntityCameraUniform>(), 128);
        assert_eq!(
            core::mem::size_of::<EntityCameraUniform>(),
            core::mem::size_of::<crate::model_pipeline::ModelCameraUniform>()
        );
        assert_eq!(core::mem::size_of::<CameraUniform>(), 80);
    }

    /// A light byte supplied per instance must survive packing unchanged, and a
    /// caller that supplies none must get the full-bright fallback rather than
    /// black.
    #[test]
    fn instance_light_packs_and_defaults_full_bright() {
        let m = glam::Mat4::IDENTITY;
        assert_eq!(EntityInstanceRaw::new(m, 0).light, 0);
        assert_eq!(EntityInstanceRaw::new(m, 0xF0).light, 0xF0);
        assert_eq!(
            EntityInstanceRaw::from_mat4(m).light,
            u32::from(crate::entity::ENTITY_FULLBRIGHT)
        );
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
