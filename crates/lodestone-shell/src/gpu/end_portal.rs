//! The end portal / end gateway star-field pass — `end_portal.wgsl`, fed by
//! [`lodestone_render::end_portal_vertices`]/[`lodestone_render::end_gateway_vertices`].
//! Its own module for the same reason [`super::sign_text`] and
//! [`super::beacon_beam`] are their own modules: this shares no mesh, no
//! bake and no batch key with [`super::block_entities`]'s cuboid-rig family
//! — the geometry is position-only and the whole visual comes from the
//! fragment shader, not from a texture atlas lookup.
//!
//! # One pipeline, not two
//!
//! Vanilla splits `RenderPipelines.END_PORTAL`/`END_GATEWAY` into two
//! pipelines sharing one shader snippet, differing only in a
//! `withShaderDefine("PORTAL_LAYERS", 15 | 16)` — a compile-time unroll
//! count, not a behavioural difference `wgpu` needs two pipeline objects to
//! express. `end_portal.wgsl` instead always runs a statically-bounded
//! 16-iteration loop and masks the 16th term's contribution by a per-vertex
//! `is_gateway` flag (0.0 for a portal, 1.0 for a gateway) — see that
//! shader's own doc for why this never risks the loop-bound-uniformity
//! hazard a *dynamic* loop count would. That means both an end portal's and
//! an end gateway's geometry can share one vertex buffer and one draw call.
//!
//! # Depth/blend, ported from `RenderPipelines.END_PORTAL_SNIPPET`
//!
//! `DepthStencilState.DEFAULT` (`GREATER_THAN_OR_EQUAL`, write **true**) and
//! no `ColorTargetState` blend override at all — this is fully opaque
//! geometry, matching every other opaque block-entity pass in this crate.
//! This renderer is reversed-Z like vanilla, so vanilla's
//! `GREATER_THAN_OR_EQUAL` is this engine's `DEPTH_COMPARE_NEARER_OR_EQUAL`,
//! transcribed with no sign flip.
//!
//! # `cull_mode: None`, deliberately
//!
//! The real jar's `END_PORTAL_SNIPPET` sets no explicit cull state (vanilla
//! defaults to back-face culling), but this pass disables culling outright.
//! [`lodestone_render::end_portal`]'s `FaceInfo`-derived winding was
//! transcribed rather than independently re-derived, and getting a
//! direction's four corners backwards would make that face's triangles wind
//! the wrong way — with back-face culling on, that reads as "this face
//! silently draws nothing", the exact island shape `CLAUDE.md` warns about,
//! and far worse than the true cost of disabling culling (at most 12 extra
//! triangles per instance, this pass's entire geometry budget).
//!
//! # Two texture bind groups, both required
//!
//! `Sampler0`/`Sampler1` (`end_sky.png`/`end_portal.png`) are separate
//! `wgpu::BindGroup`s in groups 1/2, alongside the camera/`GameTime` uniform
//! in group 0 — three total, well inside the 4-bind-group floor
//! `CLAUDE.md`'s rendering-constraints section names for exactly this kind
//! of multi-sampler pass. Both textures load together
//! ([`crate::resources::load_end_portal_textures`]) or not at all — see that
//! function's doc for why a partial load has no sensible degraded path.

use lodestone_render::{
    DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT, EndGatewaySpawn, EndPortalSpawn, EndPortalVertex, end_gateway_vertices,
    end_portal_vertices,
};

/// Mirrors [`lodestone_render::EndPortalVertex`] field-for-field as a
/// GPU-layout type, the same "own vertex type per pass" idiom
/// `gpu/sign_text.rs`/`gpu/beacon_beam.rs` already use.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    is_gateway: f32,
}

impl From<EndPortalVertex> for GpuVertex {
    fn from(v: EndPortalVertex) -> Self {
        Self {
            position: v.position,
            is_gateway: if v.is_gateway { 1.0 } else { 0.0 },
        }
    }
}

/// `camera.view_proj` (64 bytes) + `game_time` (4) + 12 bytes of pad to keep
/// the buffer a multiple of 16, matching every other `uniform`-block layout
/// in this codebase.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
    game_time: f32,
    _pad: [f32; 3],
}

/// Fixed vertex capacity: an end portal contributes at most 12 vertices, an
/// end gateway at most 36 (all six faces). 6,000 comfortably covers every
/// portal/gateway a player could plausibly have loaded at once.
const MAX_VERTICES: usize = 6_000;

/// Draws the end portal / end gateway star-field effect — see the module doc
/// for the one-pipeline shape.
#[derive(Debug)]
pub(super) struct EndPortalRenderer {
    pipeline: wgpu::RenderPipeline,
    cam_bind_group: wgpu::BindGroup,
    cam_uniform: wgpu::Buffer,
    /// `None` off a jar-less run or a pack missing either texture — same
    /// fail-open contract as [`super::sign_text::SignTextRenderer::font`].
    textures: Option<(wgpu::BindGroup, wgpu::BindGroup)>,
    vertices: wgpu::Buffer,
}

impl EndPortalRenderer {
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-end-portal-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/end_portal.wgsl").into()),
        });

        let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-end-portal-cam-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let cam_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-end-portal-cam-uniform"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-end-portal-cam-bg"),
            layout: &cam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_uniform.as_entire_binding(),
            }],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-end-portal-texture-bgl"),
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

        let textures = crate::resources::load_end_portal_textures().map(|(sky, portal)| {
            let make_bind_group = |label: &str, image: &lodestone_assets::Image| {
                let view = super::entities::entity_texture_from_image(device, queue, image);
                let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some(label),
                    address_mode_u: wgpu::AddressMode::Repeat,
                    address_mode_v: wgpu::AddressMode::Repeat,
                    address_mode_w: wgpu::AddressMode::ClampToEdge,
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                });
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &texture_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&sampler),
                        },
                    ],
                })
            };
            (
                make_bind_group("lodestone-end-portal-sky-bg", &sky),
                make_bind_group("lodestone-end-portal-portal-bg", &portal),
            )
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-end-portal-layout"),
            bind_group_layouts: &[Some(&cam_layout), Some(&texture_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32],
        })];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-end-portal-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &vertex_buffers,
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
                // See the module doc: winding is transcribed, not re-derived,
                // and a backwards face should draw wrong rather than vanish.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(DEPTH_COMPARE_NEARER_OR_EQUAL),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-end-portal-vertices"),
            size: (MAX_VERTICES * std::mem::size_of::<GpuVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            cam_bind_group,
            cam_uniform,
            textures,
            vertices,
        }
    }

    /// Uploads this frame's view-projection, game-time clock and resolved
    /// instance geometry. Must run before the render pass opens. Returns the
    /// vertex count to pass to [`draw`](Self::draw), capped at
    /// [`MAX_VERTICES`].
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        game_time: f32,
        portals: &[EndPortalSpawn],
        gateways: &[EndGatewaySpawn],
    ) -> u32 {
        queue.write_buffer(
            &self.cam_uniform,
            0,
            bytemuck::bytes_of(&CameraUniform {
                view_proj: *view_proj,
                game_time,
                _pad: [0.0; 3],
            }),
        );
        if self.textures.is_none() {
            return 0;
        }

        let mut verts = Vec::new();
        for portal in portals {
            verts.extend(end_portal_vertices(portal.pos).into_iter().map(GpuVertex::from));
        }
        for gateway in gateways {
            verts.extend(
                end_gateway_vertices(gateway.pos, &gateway.faces)
                    .into_iter()
                    .map(GpuVertex::from),
            );
        }

        let len = verts.len().min(MAX_VERTICES);
        if len > 0 {
            queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&verts[..len]));
        }
        len as u32
    }

    /// Records the draw — opaque, depth-writing, so it belongs with the rest
    /// of the pass's opaque/cutout geometry, before translucent water.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        let Some((sky, portal)) = &self.textures else {
            return;
        };
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, sky, &[]);
        pass.set_bind_group(2, portal, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..count, 0..1);
    }
}
