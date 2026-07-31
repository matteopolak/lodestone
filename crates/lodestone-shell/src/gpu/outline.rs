//! The mining-crack target descriptor and the block-outline wireframe pass.
use lodestone_render::DEPTH_FORMAT;

/// The block currently being mined, for the progressive crack overlay: its world
/// position, vanilla state id (to resolve the block's real model geometry) and
/// destruction stage `0..=9`. Passed to [`RenderState::render_with_crack`].
#[derive(Debug, Clone, Copy)]
pub struct CrackTarget {
    /// World block position of the target.
    pub block: [i32; 3],
    /// Vanilla state id, used to resolve the block's baked quads.
    pub state_id: u32,
    /// Destruction stage `0..=9`; selects the `destroy_stage_N` sprite.
    pub stage: u8,
}

/// The 12 edges of a unit cube as pairs of corner indices (line list).
const CUBE_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 3),
    (3, 2),
    (2, 0), // bottom face
    (4, 5),
    (5, 7),
    (7, 6),
    (6, 4), // top face
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7), // verticals
];

/// Draws a black wireframe box around the targeted block. Its own pipeline
/// (line-list topology, `LessEqual` depth, no depth write, alpha-blended) so it
/// reads clearly over terrain without a second pass or z-fighting.
#[derive(Debug)]
pub(super) struct OutlineRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
}

impl OutlineRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-outline-shader"),
            source: wgpu::ShaderSource::Wgsl(
                r"
struct Uniform { view_proj: mat4x4<f32> };
@group(0) @binding(0) var<uniform> u: Uniform;

@vertex
fn vs_main(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return u.view_proj * vec4<f32>(pos, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.6);
}
"
                .into(),
            ),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-outline-bgl"),
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

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-outline-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        // 24 vertices (12 edges × 2), 3 f32 each.
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-vertices"),
            size: (24 * 3 * std::mem::size_of::<f32>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-outline-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-outline-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (3 * std::mem::size_of::<f32>()) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    }],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group,
            uniform,
            vertices,
        }
    }

    /// Upload the view-projection and the box vertices for `block` (slightly
    /// expanded so the lines sit just outside the block faces). Must be called
    /// before the render pass begins — buffers can't be written mid-pass.
    /// `outline` is the block's real outline shape in world space, from
    /// [`LiveCollision::outline_boxes_at`]. Pass an empty slice to fall back to a
    /// unit cube — correct for the demo palette, which has no outline census and
    /// is all full cubes anyway.
    ///
    /// Vanilla draws the *outline* shape here, which is a third thing distinct
    /// from collision and from fluid presence: only 3,328 of 32,366 block states
    /// have a full-cube outline, so a hardcoded cube is wrong for roughly nine
    /// states in ten. A slab's box is half height and kelp's is a thin column,
    /// and neither matches its collision shape — kelp has none at all.
    ///
    /// Multiple boxes are unioned into their bounds rather than drawn
    /// separately. That is not vanilla-exact for multi-box shapes like a fence
    /// (vanilla outlines each box), but it is a strict improvement on a unit cube
    /// and needs no change to the line-list geometry below.
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        block: [i32; 3],
        outline: &[lodestone_physics::Aabb],
    ) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));

        const PAD: f32 = 0.002;
        let (mut lo, mut hi) = (
            [
                block[0] as f32 - PAD,
                block[1] as f32 - PAD,
                block[2] as f32 - PAD,
            ],
            [
                block[0] as f32 + 1.0 + PAD,
                block[1] as f32 + 1.0 + PAD,
                block[2] as f32 + 1.0 + PAD,
            ],
        );
        if let Some((first, rest)) = outline.split_first() {
            let mut b = *first;
            for o in rest {
                b.min_x = b.min_x.min(o.min_x);
                b.min_y = b.min_y.min(o.min_y);
                b.min_z = b.min_z.min(o.min_z);
                b.max_x = b.max_x.max(o.max_x);
                b.max_y = b.max_y.max(o.max_y);
                b.max_z = b.max_z.max(o.max_z);
            }
            lo = [
                b.min_x as f32 - PAD,
                b.min_y as f32 - PAD,
                b.min_z as f32 - PAD,
            ];
            hi = [
                b.max_x as f32 + PAD,
                b.max_y as f32 + PAD,
                b.max_z as f32 + PAD,
            ];
        }
        // Corner index bit layout: x = bit0, y = bit1, z = bit2.
        let corner = |i: usize| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        };
        let mut verts = [0f32; 24 * 3];
        for (e, &(a, b)) in CUBE_EDGES.iter().enumerate() {
            let ca = corner(a);
            let cb = corner(b);
            let base = e * 6;
            verts[base..base + 3].copy_from_slice(&ca);
            verts[base + 3..base + 6].copy_from_slice(&cb);
        }
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&verts));
    }

    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..24, 0..1);
    }
}
