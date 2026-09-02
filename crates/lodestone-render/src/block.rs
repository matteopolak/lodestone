//! The block render pass: a real depth-tested pipeline that consumes
//! [`PackedVertex`] geometry, samples the atlas, and shades with per-vertex AO
//! and light.
//!
//! This is the end-to-end target the mesher feeds. It owns:
//!
//! * a [`DepthBuffer`] (Depth32Float) so nearer geometry occludes farther,
//! * a camera uniform (`view_proj` plus a per-section world origin),
//! * an atlas bind group (texture + sampler + a sprite-UV table), and
//! * a pipeline with back-face culling (`FrontFace::Ccw`, cull `Back`) — correct
//!   because [`face_winding_is_outward`](crate::mesh::face_winding_is_outward)
//!   guarantees outward winding.
//!
//! The shader unpacks the two-word vertex, reconstructs position/normal/uv/AO/
//! light, looks the sprite's UV rectangle up in a storage buffer, samples the
//! mipped atlas, and multiplies by an AO/light term.
//!
//! ## Greedy-quad texturing
//!
//! For the reference (per-face) mesher each quad is one tile, so the tile
//! coordinate is in `0..1` and maps straight onto the sprite rect. A *greedy*
//! merged quad spans many tiles, so its tile coordinate runs `0..w`/`0..h`. The
//! fragment stage wraps that coordinate with `fract` into the sprite's atlas
//! sub-rect, tiling the one sprite across the merged span instead of running the
//! UV off the sprite into its atlas neighbours. Mip derivatives are taken from
//! the *continuous* (pre-wrap) tile coordinate via `textureSampleGrad` so the
//! per-tile seams don't collapse mip selection. The mesher already restricts
//! merges to a single sprite (see `QuadKey`), so this shader-side wrap is the
//! only piece the atlas path needs.
//!
//! A `texture_2d_array` layer-per-sprite layout (hardware `REPEAT`, no `fract`)
//! is **not** an option: the vanilla block atlas has 1269 sprites, far past the
//! 256 array layers WebGPU guarantees, so a single 2D atlas is mandatory and the
//! shader-side wrap is the *only* portable path (§12.14).

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::mesh::Mesh;
use crate::texture::GpuAtlas;
use crate::vertex::PackedVertex;

/// Depth format used by the block pass.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The value a depth attachment clears to, under this renderer's **reversed-Z**
/// projection: the far plane, which is `0.0`.
///
/// Every `LoadOp::Clear` on a depth attachment must use this rather than a
/// literal, because the literal is the half of a reversed-Z conversion that
/// compiles, validates and silently discards the entire frame — a pass clearing
/// to `1.0` under a `Greater` comparison rejects every fragment and draws
/// nothing at all. See [`Camera::projection_matrix`](crate::Camera::projection_matrix).
pub const DEPTH_CLEAR: f32 = 0.0;

/// "Strictly nearer wins" — vanilla's `GREATER_THAN`, and what a forward `[0,1]`
/// buffer spells `Less`.
///
/// Reversed-Z puts the near plane at `1.0`, so *nearer* is *greater*. Named
/// after the property rather than the operator so a reader does not have to
/// re-derive the direction at each pipeline.
pub const DEPTH_COMPARE_NEARER: wgpu::CompareFunction = wgpu::CompareFunction::Greater;

/// "Nearer or exactly tied wins" — vanilla's `GREATER_THAN_OR_EQUAL`, which is
/// its `DepthStencilState.DEFAULT`, and what a forward `[0,1]` buffer spells
/// `LessEqual`.
///
/// The `Equal` half is load-bearing wherever a model places two elements at
/// identical coordinates and relies on the later one winning — a grass block's
/// tinted overlay is the standing example. Substituting
/// [`DEPTH_COMPARE_NEARER`] there loses every such quad.
pub const DEPTH_COMPARE_NEARER_OR_EQUAL: wgpu::CompareFunction =
    wgpu::CompareFunction::GreaterEqual;

/// `view_proj` plus the world-space origin of the section being drawn, in one
/// non-dynamic uniform.
///
/// **No longer the block shader's own group-0 layout** — issue #76 split that
/// into [`shared_camera_buffer`] (per frame) plus a dynamic-offset
/// [`SectionOriginUniform`](crate::model_pipeline::SectionOriginUniform) (per
/// section, written once at upload), because holding both in one struct forced a
/// whole-struct `queue.write_buffer` per section per frame just to re-aim the
/// camera. Kept because it is still the first 80 bytes of
/// [`ModelCameraUniform`](crate::model_pipeline::ModelCameraUniform), which the
/// crack-overlay pipeline uses (one mesh, one origin, so the split buys it
/// nothing).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct CameraUniform {
    /// Column-major view-projection matrix.
    pub view_proj: [[f32; 4]; 4],
    /// World-space origin of the current section, `w` unused.
    pub section_origin: [f32; 4],
}

impl CameraUniform {
    /// Build from a [`Camera`](crate::camera::Camera) and a section origin.
    #[must_use]
    pub fn new(camera: &crate::camera::Camera, section_origin: [f32; 3]) -> Self {
        CameraUniform {
            view_proj: camera.view_projection().to_cols_array_2d(),
            section_origin: [section_origin[0], section_origin[1], section_origin[2], 0.0],
        }
    }
}

/// A depth attachment sized to a render target.
#[derive(Debug)]
pub struct DepthBuffer {
    /// The depth texture.
    pub texture: wgpu::Texture,
    /// A view for use as a depth-stencil attachment.
    pub view: wgpu::TextureView,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl DepthBuffer {
    /// Create a depth buffer of the given size.
    #[must_use]
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lodestone-depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        DepthBuffer {
            texture,
            view,
            width,
            height,
        }
    }
}

/// GPU vertex + index buffers for one mesh.
#[derive(Debug)]
pub struct GpuMesh {
    /// Packed vertex buffer.
    pub vertices: wgpu::Buffer,
    /// `u32` index buffer.
    pub indices: wgpu::Buffer,
    /// Number of indices to draw.
    pub index_count: u32,
}

impl GpuMesh {
    /// Upload a [`Mesh`] to the GPU. Returns `None` for an empty mesh (nothing
    /// to draw).
    #[must_use]
    pub fn upload(device: &wgpu::Device, mesh: &Mesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-mesh-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-mesh-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuMesh {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
        })
    }
}

/// The block render pipeline plus its bind-group layouts.
#[derive(Debug)]
pub struct BlockPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the camera uniform (group 0).
    pub camera_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the atlas + sprite table (group 1).
    pub atlas_layout: wgpu::BindGroupLayout,
}

impl BlockPipeline {
    /// Build the opaque block pipeline targeting `color_format` with depth
    /// testing and depth writes (the `Solid` layer).
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        Self::for_layer(
            device,
            color_format,
            crate::translucency::RenderLayer::Solid,
        )
    }

    /// Build the pipeline for a specific [`RenderLayer`](crate::translucency::RenderLayer).
    ///
    /// * `Solid`/`Cutout` — opaque target, depth-write on, back-face culled.
    /// * `Translucent` — straight alpha blending, depth **test** on but depth
    ///   **write off** (so translucent quads don't occlude each other), and no
    ///   back-face culling (glass/water panes are seen from both sides). The
    ///   caller supplies quads already sorted back-to-front via
    ///   [`TranslucentMesh`](crate::translucency::TranslucentMesh).
    #[must_use]
    pub fn for_layer(
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        layer: crate::translucency::RenderLayer,
    ) -> Self {
        use crate::translucency::RenderLayer;
        let translucent = layer == RenderLayer::Translucent;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-block-shader"),
            source: wgpu::ShaderSource::Wgsl(BLOCK_WGSL.into()),
        });

        // Two bindings inside group 0 — the shared per-frame camera + fog, and
        // the per-section origin behind a dynamic offset. A second *binding*,
        // not a fifth bind *group*: this pipeline still spends exactly two
        // (camera + atlas), well under wgpu's portable `max_bind_groups` floor
        // of 4.
        //
        // This is issue #76: the packed path used to hold `view_proj` **and**
        // the origin in one non-dynamic uniform, which forced a whole-struct
        // `queue.write_buffer` per section per frame just to re-aim the camera —
        // the shape issue #75 measured at 52.9% of main-thread CPU on the model
        // path. The origin is constant for a section's lifetime, so it belongs
        // behind a dynamic offset written once at upload. See
        // `docs/section-camera-uniform.md`.
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-camera-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Vertex reads the view-projection and the sky-darken lane;
                    // fragment reads the fog block (eye, colour, range), so both
                    // stages bind it — the same visibility `ModelPipeline`'s own
                    // camera group uses. This was `VERTEX` only between issues
                    // #76 and #400: the struct carried the fog lanes before the
                    // shader read them.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    // The origin only ever feeds `world = position + origin.xyz`
                    // in the vertex stage.
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            core::mem::size_of::<crate::model_pipeline::SectionOriginUniform>()
                                as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-atlas-bgl"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-block-layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-block-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(PackedVertex::vertex_layout())],
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
                depth_compare: Some(DEPTH_COMPARE_NEARER),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        BlockPipeline {
            pipeline,
            camera_layout,
            atlas_layout,
        }
    }

    /// Create the group-0 bind group from the shared per-frame buffer (binding
    /// 0: view-projection + fog, built with [`shared_camera_buffer`]) and an
    /// origin buffer (binding 1, dynamic offset).
    ///
    /// `origin_buffer` may be a single
    /// [`SectionOriginUniform`](crate::model_pipeline::SectionOriginUniform)
    /// slot (one-off draws, a test's synthetic section) or a large arena backing
    /// many sections at different offsets — the *window* bound here is always
    /// one `SectionOriginUniform` (16 bytes). A caller addressing many sections
    /// through one arena builds this bind group **once** and picks a section by
    /// the dynamic offset passed to `set_bind_group`, never by rebuilding the
    /// bind group. That is the whole point of issue #76: one bind group and one
    /// per-frame write for the whole packed table, however many sections are
    /// resident.
    ///
    /// This is the same shape (and the same two backing types) as
    /// [`ModelPipeline::camera_bind_group`](crate::ModelPipeline::camera_bind_group).
    #[must_use]
    pub fn camera_bind_group(
        &self,
        device: &wgpu::Device,
        shared_buffer: &wgpu::Buffer,
        origin_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-camera-bg"),
            layout: &self.camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: shared_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: origin_buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(
                            core::mem::size_of::<crate::model_pipeline::SectionOriginUniform>()
                                as u64,
                        ),
                    }),
                },
            ],
        })
    }

    /// Create the atlas bind group from an atlas and a sprite-UV storage buffer.
    ///
    /// Each sprite entry is `vec4(uv_min.x, uv_min.y, uv_size.x, uv_size.y)`.
    #[must_use]
    pub fn atlas_bind_group(
        &self,
        device: &wgpu::Device,
        atlas: &GpuAtlas,
        sprite_uv_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-atlas-bg"),
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
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: sprite_uv_buffer.as_entire_binding(),
                },
            ],
        })
    }
}

/// WGSL for the block pass: unpacks the three-word vertex, transforms, samples
/// the atlas, and shades by smooth AO and light.
const BLOCK_WGSL: &str = include_str!("shaders/block.wgsl");

/// Build a sprite-UV storage buffer from `(uv_min, uv_size)` rectangles.
#[must_use]
pub fn sprite_uv_buffer(device: &wgpu::Device, rects: &[[f32; 4]]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-sprite-uv"),
        contents: bytemuck::cast_slice(rects),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

/// Build the packed path's **shared** group-0 buffer (binding 0): this frame's
/// view-projection plus its fog block, written **once per frame** however many
/// sections are resident. Rewrite it with
/// [`update_model_shared_camera_buffer`](crate::update_model_shared_camera_buffer),
/// which takes any buffer of this layout.
///
/// The contents are a [`ModelSharedCameraUniform`](crate::ModelSharedCameraUniform)
/// — deliberately the *same* type the model/fluid pipelines use rather than a
/// twin. Two structurally identical fog uniforms is exactly the drift this
/// crate's shaders warn about ("entities and terrain disagree about what time it
/// is"); only the buffer label differs, so a capture names the path it came
/// from.
///
/// Replaces the old `camera_buffer(device, CameraUniform)`, whose single
/// non-dynamic `{ view_proj, section_origin }` uniform is what forced a
/// whole-struct write per section per frame — issue #76.
#[must_use]
pub fn shared_camera_buffer(
    device: &wgpu::Device,
    view_proj: [[f32; 4]; 4],
    fog: crate::fog::FogUniform,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("lodestone-packed-shared-camera-uniform"),
        contents: bytemuck::bytes_of(&crate::model_pipeline::ModelSharedCameraUniform {
            view_proj,
            fog,
        }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_uniform_is_pod_and_sized() {
        // 64 bytes matrix + 16 bytes origin.
        assert_eq!(core::mem::size_of::<CameraUniform>(), 80);
    }
}
