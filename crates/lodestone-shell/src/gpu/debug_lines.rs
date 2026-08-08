//! The world-space debug-line pass (`docs/plugin-api.md`'s `ExtractSet::Debug`
//! channel) and the polled source that feeds it.
use lodestone_render::DEPTH_FORMAT;

/// One coloured vertex of a world-space debug line segment — the render half
/// of `lodestone_ecs::player::DebugLine` (`docs/plugin-api.md`'s
/// `ExtractSet::Debug` channel). A separate, `bytemuck`-friendly type rather
/// than reusing the ECS one directly, so this module (and `wgpu`) never has
/// to care whether the ECS type's layout is `f32` or `f64` — see
/// [`debug_line_vertices`] for the conversion.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DebugLineVertex {
    /// World-space position.
    pub position: [f32; 3],
    /// Linear RGBA, `0.0..=1.0`.
    pub color: [f32; 4],
}

/// Lower a plugin's world-space debug segments
/// (`lodestone_ecs::player::DebugLine`) into the vertex pairs
/// [`DebugLineRenderer`] draws. The one piece of glue between the ECS
/// channel and this pass — see [`DebugLinesSource`]'s docs for why installing
/// it is the one wire this crate cannot lay itself.
#[must_use]
pub fn debug_line_vertices(lines: &[lodestone_ecs::player::DebugLine]) -> Vec<DebugLineVertex> {
    lines
        .iter()
        .flat_map(|line| {
            let start = [
                line.start.x as f32,
                line.start.y as f32,
                line.start.z as f32,
            ];
            let end = [line.end.x as f32, line.end.y as f32, line.end.z as f32];
            [
                DebugLineVertex {
                    position: start,
                    color: line.color,
                },
                DebugLineVertex {
                    position: end,
                    color: line.color,
                },
            ]
        })
        .collect()
}

/// Append the twelve edges of the axis-aligned box `(min, max)` in `color`.
///
/// The one primitive both F3 sub-modes below are built from — a box is twelve
/// segments, which at [`MAX_DEBUG_LINE_SEGMENTS`] leaves room for ~340 of them.
fn push_box(out: &mut Vec<DebugLineVertex>, min: [f32; 3], max: [f32; 3], color: [f32; 4]) {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    let corner = |x: f32, y: f32, z: f32| DebugLineVertex {
        position: [x, y, z],
        color,
    };
    // Bottom ring, top ring, then the four uprights joining them.
    let edges = [
        ((x0, y0, z0), (x1, y0, z0)),
        ((x1, y0, z0), (x1, y0, z1)),
        ((x1, y0, z1), (x0, y0, z1)),
        ((x0, y0, z1), (x0, y0, z0)),
        ((x0, y1, z0), (x1, y1, z0)),
        ((x1, y1, z0), (x1, y1, z1)),
        ((x1, y1, z1), (x0, y1, z1)),
        ((x0, y1, z1), (x0, y1, z0)),
        ((x0, y0, z0), (x0, y1, z0)),
        ((x1, y0, z0), (x1, y1, z0)),
        ((x1, y0, z1), (x1, y1, z1)),
        ((x0, y0, z1), (x0, y1, z1)),
    ];
    for ((ax, ay, az), (bx, by, bz)) in edges {
        out.push(corner(ax, ay, az));
        out.push(corner(bx, by, bz));
    }
}

/// F3+B: one wireframe box per entity, plus a short forward ray from eye height
/// showing where it is looking (issue #197).
///
/// The box comes from the **jar-derived** dimension census
/// (`lodestone_data::entity_dimensions`), scaled by the draw's own `scale`, and
/// is centred horizontally on `feet` exactly as `EntityDimensions` does — the
/// same source `gpu/nametag.rs` uses for the nametag anchor, so a hitbox and a
/// nametag can never disagree about how tall an entity is. An entity whose type
/// path the census cannot resolve contributes **no box**, rather than a
/// plausible-looking default one: a wrong hitbox is worse than a missing one,
/// because the whole point of the overlay is to be believed.
///
/// Vanilla's colour is per-part (`white` for the hitbox, `cyan` for the eye
/// ray); this draws the hitbox white and the ray cyan for the same reason.
#[must_use]
pub fn entity_hitbox_vertices(draws: &[crate::entities::EntityDraw]) -> Vec<DebugLineVertex> {
    const HITBOX: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const EYE_RAY: [f32; 4] = [0.0, 1.0, 1.0, 1.0];
    /// How far the look ray extends, in blocks — vanilla's
    /// `EntityRenderer.renderHitbox` draws `2.0`.
    const RAY_LEN: f32 = 2.0;

    let mut out = Vec::new();
    for draw in draws {
        let Some(dims) = lodestone_data::entity_types::entity_type_id_parts(
            "minecraft",
            &draw.type_path,
        )
        .and_then(lodestone_data::entity_dimensions::base_dimensions)
        else {
            continue;
        };
        let half = dims.width * draw.scale * 0.5;
        let height = dims.height * draw.scale;
        if half <= 0.0 || height <= 0.0 {
            continue;
        }
        let f = draw.feet;
        push_box(
            &mut out,
            [f.x - half, f.y, f.z - half],
            [f.x + half, f.y + height, f.z + half],
            HITBOX,
        );

        // The look ray, from eye height along the head's yaw/pitch. Minecraft's
        // yaw is measured from +Z and increases clockwise, which is the same
        // convention `DebugStats::facing` documents.
        let eye_y = f.y + height * 0.85;
        let (yaw, pitch) = (draw.head_yaw.to_radians(), draw.pitch.to_radians());
        let dir = glam::Vec3::new(
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        );
        out.push(DebugLineVertex {
            position: [f.x, eye_y, f.z],
            color: EYE_RAY,
        });
        out.push(DebugLineVertex {
            position: [
                f.x + dir.x * RAY_LEN,
                eye_y + dir.y * RAY_LEN,
                f.z + dir.z * RAY_LEN,
            ],
            color: EYE_RAY,
        });
    }
    out
}

/// F3+G: the borders of the chunk the player is standing in (issue #197).
///
/// Vanilla's `LevelRenderer.renderChunkBorders` draws the column's own edges
/// plus a horizontal ring at each section boundary. This draws the four corner
/// uprights and the outline of every 16-block section slab, over
/// `min_y..min_y + height` — the **real** world column, passed in by the caller
/// rather than assumed, because a nether or custom-height dimension has a
/// different range and a hardcoded `-64..320` would silently draw the wrong box
/// there.
///
/// Segment count is `4 + 4 * sections`, so a 24-section overworld column is 100
/// segments — comfortably inside [`MAX_DEBUG_LINE_SEGMENTS`] alongside a screen
/// of hitboxes.
#[must_use]
pub fn chunk_border_vertices(
    player: [f64; 3],
    min_y: i32,
    height: u32,
) -> Vec<DebugLineVertex> {
    /// Vanilla's chunk-edge yellow.
    const EDGE: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
    /// The per-section rings, dimmer so the column's own edges read first.
    const SECTION: [f32; 4] = [0.25, 0.25, 1.0, 1.0];

    let cx = (player[0].floor() as i32).div_euclid(16);
    let cz = (player[2].floor() as i32).div_euclid(16);
    let (x0, z0) = ((cx * 16) as f32, (cz * 16) as f32);
    let (x1, z1) = (x0 + 16.0, z0 + 16.0);
    let y0 = min_y as f32;
    let y1 = y0 + height as f32;

    let mut out = Vec::new();
    // The four uprights, full column height.
    for (x, z) in [(x0, z0), (x1, z0), (x1, z1), (x0, z1)] {
        out.push(DebugLineVertex {
            position: [x, y0, z],
            color: EDGE,
        });
        out.push(DebugLineVertex {
            position: [x, y1, z],
            color: EDGE,
        });
    }
    // A ring at every section boundary, including both ends. The end rings take
    // the edge colour so the column reads as a closed box.
    let sections = (height / 16).max(1);
    for s in 0..=sections {
        let y = y0 + (s * 16) as f32;
        if y > y1 {
            break;
        }
        let colour = if s == 0 || y >= y1 { EDGE } else { SECTION };
        let ring = [
            ((x0, z0), (x1, z0)),
            ((x1, z0), (x1, z1)),
            ((x1, z1), (x0, z1)),
            ((x0, z1), (x0, z0)),
        ];
        for ((ax, az), (bx, bz)) in ring {
            out.push(DebugLineVertex {
                position: [ax, y, az],
                color: colour,
            });
            out.push(DebugLineVertex {
                position: [bx, y, bz],
                color: colour,
            });
        }
    }
    out
}

/// Fixed capacity for the debug-line pass, in line segments (two vertices
/// each). A debug overlay does not need to grow without bound the way
/// [`crate::particles::ParticleRenderer`]'s instance count does — a few
/// thousand segments is far more than one pathfinder's route — so this stays
/// a **fixed** buffer, like [`OutlineRenderer`]'s, rather than the
/// grow-and-reallocate pattern particles use. That choice is what lets
/// [`DebugLineRenderer::prepare`] take `&self`: [`RenderState::render`] itself
/// takes `&self` (it is called through a shared reference from the frame
/// loop), so a `prepare` that needed to reallocate would need `&mut self` and
/// a second, `app.rs`-level call before every frame — exactly the wiring this
/// crate cannot add (see [`DebugLinesSource`]). Beyond this many segments,
/// [`DebugLineRenderer::prepare`] truncates rather than growing.
pub(super) const MAX_DEBUG_LINE_SEGMENTS: usize = 4096;

/// Draws arbitrary coloured world-space line segments — a pathfinder's
/// planned route, a reachability probe, anything a plugin wants visible for
/// debugging (`CLAUDE.md`'s island rule: a subsystem with no way onto the
/// screen is undebuggable by construction).
///
/// A generalisation of [`OutlineRenderer`] immediately above: the same
/// `view_proj`-only bind group and line-list topology, but a per-vertex
/// colour instead of a hardcoded black, and an arbitrary (fixed-capacity)
/// vertex count instead of one hardcoded unit cube.
#[derive(Debug)]
pub(super) struct DebugLineRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
}

impl DebugLineRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-debug-lines-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/debug_lines.wgsl").into()),
        });

        // Same bind-group-layout shape as `OutlineRenderer`: one `view_proj`
        // uniform, nothing else. A dedicated pipeline entirely outside the
        // model shader's four bind groups, so this pass has no bearing on the
        // 4-bind-group floor `CLAUDE.md` warns about (`gpu.rs`'s own
        // `BlockPipeline`/`ModelPipeline` are untouched by this addition).
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-debug-lines-bgl"),
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
            label: Some("lodestone-debug-lines-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-debug-lines-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-debug-lines-vertices"),
            size: (MAX_DEBUG_LINE_SEGMENTS * 2 * std::mem::size_of::<DebugLineVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-debug-lines-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-debug-lines-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<DebugLineVertex>() as u64,
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            // Same depth treatment as `OutlineRenderer`: tested against
            // terrain (so a debug line behind a wall does not bleed through
            // the block in front of it) but not written, so overlapping debug
            // lines never punch depth holes in each other or in what is drawn
            // after them.
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

    /// Upload this frame's view-projection and line vertices. Must run before
    /// the render pass opens — buffers cannot be written mid-pass. Returns the
    /// vertex count actually written, capped at
    /// `2 * `[`MAX_DEBUG_LINE_SEGMENTS`] — pass it to [`draw`](Self::draw).
    ///
    /// Takes `&self`, not `&mut self`: see [`MAX_DEBUG_LINE_SEGMENTS`]'s docs
    /// for why a fixed buffer is what makes that possible.
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        vertices: &[DebugLineVertex],
    ) -> u32 {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));
        let capped = &vertices[..vertices.len().min(MAX_DEBUG_LINE_SEGMENTS * 2)];
        if capped.is_empty() {
            return 0;
        }
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(capped));
        u32::try_from(capped.len()).unwrap_or(u32::MAX)
    }

    /// Record the draw. No-op when `vertex_count` (the last
    /// [`prepare`](Self::prepare)'s return value) is zero.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, vertex_count: u32) {
        if vertex_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..vertex_count, 0..1);
    }
}

/// Polled source for this frame's world-space debug lines — the render half
/// of `ExtractSet::Debug` (`docs/plugin-api.md`). Same idiom as
/// [`OutlineShapeSource`]/[`ThirdPersonBodySource`] immediately below: the
/// renderer cannot reach the ECS `DebugLines` resource directly (this crate
/// has no dependency edge back to whoever owns the `World`), and threading it
/// through [`RenderState::render`]'s signature would touch every call site —
/// which, for this change, means `app.rs`'s `render(...)` calls, and
/// `app.rs` is out of scope for this work (a different agent holds it; see
/// `docs/plugin-api.md`).
///
/// **This is the one wire this crate cannot lay itself.** Unset — the
/// default, and the state until someone installs a source — samples to
/// nothing, so [`RenderState::render`]'s behaviour is unchanged from before
/// this existed: zero pixels from this pass until a caller installs a real
/// source with [`RenderState::set_debug_lines_source`]. The install call
/// itself is one line, e.g. (schematically — the exact accessor depends on
/// how `app.rs` reaches the `EcsHandle`):
///
/// ```text
/// render_state.set_debug_lines_source(move || {
///     let world = ecs_handle.read();
///     lodestone_render_shell::gpu::debug_line_vertices(
///         &world.resource::<lodestone_ecs::player::DebugLines>().0,
///     )
/// });
/// ```
#[derive(Default)]
pub struct DebugLinesSource(
    #[allow(clippy::type_complexity)]
    pub(super) Option<Box<dyn Fn() -> Vec<DebugLineVertex> + Send + Sync>>,
);

impl DebugLinesSource {
    #[must_use]
    pub(super) fn sample(&self) -> Vec<DebugLineVertex> {
        self.0.as_ref().map_or_else(Vec::new, |f| f())
    }
}

impl std::fmt::Debug for DebugLinesSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DebugLinesSource")
            .field(&if self.0.is_some() {
                "installed"
            } else {
                "empty"
            })
            .finish()
    }
}
