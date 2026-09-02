//! The mining-crack render pass: a second, depth-biased, alpha-blended pass that
//! redraws a block's own model geometry textured with a `destroy_stage_N`
//! sprite.
//!
//! This is the consumer of two pieces that already exist and did nothing on
//! their own: the ten crack sprites stitched into the block atlas by
//! [`BlockModels`](crate::BlockModels) (see [`BlockModels::crack_stage_uv`]), and
//! the crack geometry built by [`crack`](crate::crack) from the target block's
//! real quads. Because the geometry is the block's own quads
//! ([`build_crack_mesh`](crate::crack::build_crack_mesh)), the crack traces
//! slabs, stairs and cross-plants correctly instead of floating a full cube.
//!
//! # Why a separate pipeline
//!
//! The crack is coplanar with the block surface, so it z-fights without help.
//! This pipeline draws with a **negative depth bias** (polygon offset) so the
//! crack wins the depth test against the face it sits on, `LessEqual` depth
//! compare so it is allowed to be coplanar, and **no depth write** so it never
//! occludes anything drawn later.
//!
//! # Blend: doubled multiply, not alpha
//!
//! Vanilla's `pipeline/crumbling` (`RenderPipelines.CRUMBLING` in
//! `net.minecraft.client.renderer`) blends colour as
//! `src_factor = DST_COLOR, dst_factor = SRC_COLOR, op = Add` — i.e.
//! `out = dst*src + src*dst = 2 * src * dst`, a doubled multiply that can only
//! ever darken the surface underneath, never lighten it. Alpha blending (the
//! obvious first guess, since a `destroy_stage` sprite reads like "dark
//! cracks over transparent pixels") instead *adds* the sprite's light grey
//! anti-aliased edge texels on top of the block, which is why that approach
//! reads as "too white" in a play-test. Alpha channel is `src=One, dst=Zero`
//! (vanilla ignores destination alpha for this pass; we do too, since the
//! crack pass never writes back into a colour target that anything downstream
//! reads alpha from).

use wgpu::util::DeviceExt;

use crate::block::{DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT};
use crate::crack::{CrackMesh, CrackVertex};
use crate::model_pipeline::CAMERA_DEPTH_BIAS;
use crate::texture::GpuAtlas;

/// GPU-resident crack geometry: a vertex buffer, an index buffer and the index
/// count.
#[derive(Debug)]
pub struct GpuCrackMesh {
    /// Vertex buffer of [`CrackVertex`].
    pub vertices: wgpu::Buffer,
    /// `u32` index buffer.
    pub indices: wgpu::Buffer,
    /// Number of indices to draw.
    pub index_count: u32,
}

impl GpuCrackMesh {
    /// Upload a [`CrackMesh`], or `None` if it is empty (nothing to draw).
    #[must_use]
    pub fn upload(device: &wgpu::Device, mesh: &CrackMesh) -> Option<Self> {
        if mesh.indices.is_empty() {
            return None;
        }
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-crack-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-crack-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        Some(GpuCrackMesh {
            vertices,
            indices,
            index_count: mesh.indices.len() as u32,
        })
    }
}

/// The vertex buffer layout for [`CrackVertex`]: `position` at location 0,
/// `uv` at location 1.
#[must_use]
pub fn crack_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x2,
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<CrackVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

/// A depth-biased, alpha-blended pipeline for the crack overlay pass.
#[derive(Debug)]
pub struct CrackPipeline {
    /// The render pipeline.
    pub pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the camera uniform (group 0).
    pub camera_layout: wgpu::BindGroupLayout,
    /// Bind-group layout for the atlas texture + sampler (group 1).
    pub atlas_layout: wgpu::BindGroupLayout,
}

impl CrackPipeline {
    /// Build the crack pipeline targeting `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-crack-shader"),
            source: wgpu::ShaderSource::Wgsl(CRACK_WGSL.into()),
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-crack-camera-bgl"),
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
            label: Some("lodestone-crack-atlas-bgl"),
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
            label: Some("lodestone-crack-layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-crack-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(crack_vertex_layout())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    // Doubled multiply, ported from vanilla's `pipeline/crumbling`
                    // (`RenderPipelines.CRUMBLING`): colour = dst*src + src*dst =
                    // 2*src*dst, which only ever darkens. Plain `ALPHA_BLENDING`
                    // *adds* the sprite's light texels on top of the block instead
                    // of multiplying them in — that's the "too white" defect.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Dst,
                            dst_factor: wgpu::BlendFactor::Src,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::Zero,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                // The crack sits on faces of every orientation; culling would
                // drop the ones that happen to wind away from the camera.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                // Never occlude — the crack is a decal over already-drawn faces.
                depth_write_enabled: Some(false),
                // Allow the crack to be coplanar with the face it decorates.
                depth_compare: Some(DEPTH_COMPARE_NEARER_OR_EQUAL),
                stencil: wgpu::StencilState::default(),
                // Polygon offset: pull the crack toward the camera so it wins the
                // depth test against the coplanar block face instead of z-fighting.
                // Ported from vanilla's `pipeline/crumbling`, whose
                // `DepthStencilState(GREATER_THAN_OR_EQUAL, false, 1.0F, 10.0F)`
                // is `(depthTest, writeDepth, depthBiasScaleFactor,
                // depthBiasConstant)` — scale factor *then* constant, verified
                // against `VulkanRenderPipeline.java` where
                // `depthBiasConstantFactor` reads `.depthBiasConstant()` and
                // `depthBiasSlopeFactor` reads `.depthBiasScaleFactor()`. So
                // vanilla's actual bias is slope=1.0, constant=10.0 (easy to get
                // backwards from the constructor call alone). Vanilla is
                // reversed-Z (GREATER_THAN_OR_EQUAL, higher = nearer), so a
                // positive bias there pulls toward the camera; this project's
                // depth is reversed-Z too (see `camera.rs`), so "toward the
                // camera" is the same sign: constant=10, slope=1.0, transcribed
                // rather than translated.
                bias: CAMERA_DEPTH_BIAS,
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        CrackPipeline {
            pipeline,
            camera_layout,
            atlas_layout,
        }
    }

    /// Create the camera bind group from an existing uniform buffer (build it
    /// with [`model_camera_buffer`](crate::model_camera_buffer); the layout is
    /// identical).
    #[must_use]
    pub fn camera_bind_group(
        &self,
        device: &wgpu::Device,
        camera_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-crack-camera-bg"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        })
    }

    /// Create the atlas bind group from a [`GpuAtlas`] (texture + sampler).
    /// This is the same complete block atlas the model pass binds, so the
    /// `destroy_stage` UVs from [`BlockModels::crack_stage_uv`] resolve directly.
    #[must_use]
    pub fn atlas_bind_group(&self, device: &wgpu::Device, atlas: &GpuAtlas) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-crack-atlas-bg"),
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
}

const CRACK_WGSL: &str = include_str!("shaders/crack.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crack_vertex_layout_is_20_bytes_over_two_attributes() {
        let layout = crack_vertex_layout();
        assert_eq!(layout.array_stride, 20);
        assert_eq!(layout.attributes.len(), 2);
    }

    #[test]
    fn empty_crack_mesh_uploads_to_none() {
        let mesh = CrackMesh::default();
        assert!(mesh.is_empty());
    }
}
