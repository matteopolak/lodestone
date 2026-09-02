//! The lightning bolt's GPU pass — `lightning_bolt.wgsl`, uploaded from
//! [`lodestone_render::lightning_bolt::lightning_bolt_vertices`]'s pure
//! geometry.
//!
//! Its own module for the reason [`super::beacon_beam`] is: this is
//! procedural, per-frame geometry with **no texture, no light and a per-vertex
//! colour**, so it shares no mesh, no bake and no batch key with the entity
//! family — and, unlike every pass in `gpu/entity_passes.rs`, it cannot reuse
//! `EntityPipeline` at all. All eight of that type's pipeline variants are
//! built over a camera + **texture** bind-group pair and seven of them go
//! through a helper that hard-wires an instanced second vertex buffer; a bolt
//! has neither a texture nor instances.
//!
//! # The blend function is the whole look
//!
//! Vanilla's own lightning render pipeline carries its own lightning blend
//! function,
//! and that blend function is `(BlendFactor.SRC_ALPHA, BlendFactor.ONE)`
//! — **additive, scaled by the source alpha**. Nothing else in this workspace
//! uses that pair: `glint_blend()` is `(Src, One)` colour with `(Zero, One)`
//! alpha and its own doc warns that reaching for a stock `ADDITIVE` is wrong.
//!
//! It matters because the bolt's own colour, `(0.45, 0.45, 0.5)` at alpha
//! `0.3`, is a dim blue-grey. What makes a bolt read as *white* is four
//! concentric shells each adding `0.3` of it on top of the last, plus the
//! four faces of each tube. Under ordinary alpha blending the same geometry
//! would come out grey and flat, which is a plausible-looking wrong answer —
//! this is the pass where "it draws, but it looks dull" means the blend state,
//! not the colour constant.
//!
//! # Depth, culling and the 128-block height
//!
//! Depth-tested and depth-writing (vanilla's own default depth-stencil state, whose
//! `GREATER_THAN_OR_EQUAL` becomes this engine's `LessEqual` per `CLAUDE.md`'s
//! reversed-Z rule), and `cull_mode: None` — a bolt is a hollow tube a player
//! can stand inside, and vanilla's own `affectedByCulling` returns `false` for
//! it.
//!
//! That last point is not only about back faces: a bolt spans **128 blocks**
//! upward from the strike, and `lodestone_data::entity_dimensions` records
//! `lightning_bolt` as having no hitbox at all, so there is no AABB a frustum
//! test could use. This pass deliberately does no culling of its own; the cost
//! ceiling is [`MAX_BOLT_VERTICES`] rather than a cull.

use lodestone_render::{DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT};
use lodestone_render::lightning_bolt::{BOLT_VERTICES, bolt_seed_for_entity, lightning_bolt_vertices};

use crate::entities::EntityDraw;

/// The entity type path a bolt arrives as, as `EntityDraw::type_path` carries
/// it (namespace stripped).
pub(super) const LIGHTNING_BOLT_TYPE_PATH: &str = "lightning_bolt";

/// Mirrors [`lodestone_render::lightning_bolt::BoltVertex`] as a GPU-layout
/// type, the same "own vertex type per pass" idiom
/// `gpu/beacon_beam.rs`/`gpu/debug_lines.rs` already use.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BoltGpuVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// Fixed vertex capacity, i.e. **eight simultaneous bolts**.
///
/// Fixed rather than grown per frame for the reason `gpu/debug_lines.rs`
/// documents: a persistent buffer written with `queue.write_buffer` is what
/// lets `prepare` take `&self` and compose with `RenderState::render`'s
/// shared-reference calling convention.
///
/// Eight is generous against what a thunderstorm actually produces — vanilla
/// strikes at most a handful of times per tick across a whole level — and a
/// ninth bolt in one frame is dropped rather than allowed to grow the buffer,
/// which is the same "cap, do not reallocate mid-frame" contract every other
/// fixed-capacity pass here keeps.
const MAX_BOLT_VERTICES: usize = BOLT_VERTICES * 8;

/// Draws lightning bolts. See the module doc for why this is not an
/// `EntityPipeline` variant.
#[derive(Debug)]
pub(super) struct LightningBoltRenderer {
    pipeline: wgpu::RenderPipeline,
    cam_bind_group: wgpu::BindGroup,
    cam_uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
}

impl LightningBoltRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-lightning-bolt-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/lightning_bolt.wgsl").into()),
        });

        let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-lightning-bolt-cam-bgl"),
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
        let cam_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-lightning-bolt-cam-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-lightning-bolt-cam-bg"),
            layout: &cam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_uniform.as_entire_binding(),
            }],
        });

        // One bind group, not two: there is no texture to bind. Well inside
        // wgpu's 4-group floor.
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-lightning-bolt-layout"),
            bind_group_layouts: &[Some(&cam_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-lightning-bolt-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<BoltGpuVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    // Vanilla's own lightning blend function = `(SRC_ALPHA, ONE)`. See the
                    // module doc: this is what turns four dim blue-grey shells
                    // into a white bolt, and a stock `ALPHA_BLENDING` here
                    // draws the same geometry looking flat and grey.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // A hollow tube the player can stand inside; vanilla's own
                // `affectedByCulling` is false for the same subject.
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
            label: Some("lodestone-lightning-bolt-vertices"),
            size: (MAX_BOLT_VERTICES * std::mem::size_of::<BoltGpuVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            cam_bind_group,
            cam_uniform,
            vertices,
        }
    }

    /// Rebuild and upload this frame's bolt geometry. Must run before the
    /// render pass opens (no buffer writes mid-pass), and returns the vertex
    /// count to hand [`draw`](Self::draw).
    ///
    /// Rebuilt every frame rather than cached per bolt, exactly as vanilla
    /// does: `submit` runs the whole walk on each call. A bolt lives a handful
    /// of ticks and there are rarely more than one or two, so the ~1.3k
    /// vertices per bolt are cheaper to regenerate than to invalidate.
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        entities: &[EntityDraw],
    ) -> u32 {
        queue.write_buffer(&self.cam_uniform, 0, bytemuck::bytes_of(view_proj));

        let mut out: Vec<BoltGpuVertex> = Vec::new();
        for draw in entities {
            if draw.type_path.as_ref() != LIGHTNING_BOLT_TYPE_PATH {
                continue;
            }
            // `invisible` is honoured for the same reason every other pass
            // here honours it, even though vanilla's bolt has no renderer-side
            // check: the shared-flags bit reaches this record and a plugin can
            // set it on anything.
            if draw.invisible {
                continue;
            }
            if out.len() + BOLT_VERTICES > MAX_BOLT_VERTICES {
                break;
            }
            let seed = bolt_seed_for_entity(draw.id);
            out.extend(lightning_bolt_vertices(seed).into_iter().map(|v| {
                BoltGpuVertex {
                    // The pure geometry is bolt-local; the entity's own
                    // position is added here. A bolt's wire position is the
                    // strike point, i.e. the *bottom* of the trunk, which is
                    // what the anchor subtraction in the walk arranges.
                    position: [
                        v.position[0] + draw.feet.x,
                        v.position[1] + draw.feet.y,
                        v.position[2] + draw.feet.z,
                    ],
                    color: v.color,
                }
            }));
        }

        let len = out.len().min(MAX_BOLT_VERTICES);
        if len > 0 {
            queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&out[..len]));
        }
        u32::try_from(len).unwrap_or(u32::MAX)
    }

    /// Record the draw. Belongs with the translucent geometry: the pass is
    /// additive, so anything drawn over it afterward would be wrong, and
    /// anything it should brighten must already be in the framebuffer.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..count, 0..1);
    }
}
