//! The world-space debug-line pass (`docs/plugin-api.md`'s `ExtractSet::Debug`
//! channel) and the polled source that feeds it.
use std::cell::{Cell, RefCell};

use lodestone_render::{DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT};

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

/// The metadata-only part of an [`EntityDraw`] that F3+B needs in addition to
/// its interpolated render transform.
///
/// `EntityDraw` deliberately stays a render-facing POD; duplicating this state
/// onto every ordinary draw just for a debug overlay would make pose and
/// attribute-scale two more values that can drift from ingest. The app builds
/// this small, F3+B-only side table from the authoritative ingest entity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EntityHitboxState {
    pub(crate) id: i32,
    pub(crate) pose: lodestone_model::EntityPose,
    /// The resolved `minecraft:scale` attribute, excluding the draw's existing
    /// baby/small-model scale.
    pub(crate) attribute_scale: f32,
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
/// segments. The GPU buffer grows when more entities enter the view, so the
/// overlay does not depend on the order in which entities become visible.
pub(crate) fn push_box(
    out: &mut Vec<DebugLineVertex>,
    min: [f32; 3],
    max: [f32; 3],
    color: [f32; 4],
) {
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
/// showing where it is looking.
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
    entity_hitbox_vertices_with_states(draws, &[])
}

/// [`entity_hitbox_vertices`] with the live metadata needed for a pose-scaled
/// player box. The no-state sibling remains for narrow render tests; production
/// F3+B always calls this form.
#[must_use]
pub(crate) fn entity_hitbox_vertices_with_states(
    draws: &[crate::entities::EntityDraw],
    states: &[EntityHitboxState],
) -> Vec<DebugLineVertex> {
    const HITBOX: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const EYE_RAY: [f32; 4] = [0.0, 1.0, 1.0, 1.0];
    /// How far the look ray extends, in blocks — vanilla's
    /// `EntityRenderer.renderHitbox` draws `2.0`.
    const RAY_LEN: f32 = 2.0;

    let mut out = Vec::new();
    for draw in draws {
        // `EntityHitboxDebugRenderer` has this as its outermost entity gate.
        // Hidden helper players must not leave a second F3+B box after their
        // renderer and nametag have been suppressed.
        if draw.invisible {
            continue;
        }
        let Some(dims) = lodestone_data::entity_types::entity_type_id_parts(
            "minecraft",
            &draw.type_path,
        )
        .and_then(lodestone_data::entity_dimensions::base_dimensions)
        else {
            continue;
        };
        let state = states.iter().find(|state| state.id == draw.id);
        let attribute_scale = state
            .map(|state| state.attribute_scale)
            .filter(|scale| scale.is_finite() && *scale > 0.0)
            .unwrap_or(1.0);
        let effective_scale = draw.scale * attribute_scale;
        let (width, height, eye_height) = if draw.type_path.as_ref() == "player" {
            player_hitbox_metrics(
                state.map_or(lodestone_model::EntityPose::Standing, |state| state.pose),
                effective_scale,
            )
        } else {
            (
                dims.width * effective_scale,
                dims.height * effective_scale,
                dims.height * effective_scale * 0.85,
            )
        };
        let half = width * 0.5;
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

        // The look ray starts at vanilla's own get-eye-height accessor, not a fraction of the
        // bounding-box height. Player poses explicitly override it: standing is
        // 1.62 rather than 1.53, crouching is 1.27, and the three prone poses
        // are 0.4. Minecraft's yaw is measured from +Z and increases clockwise,
        // which is the same convention `DebugStats::facing` documents.
        let eye_y = f.y + eye_height;
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

/// `Avatar.POSES` / `LivingEntity.getDimensions`: the player-specific box and
/// eye-height pair for F3+B. `SLEEPING` and `DYING` are fixed dimensions, so
/// unlike every scalable pose they deliberately do not consume
/// `minecraft:scale`.
fn player_hitbox_metrics(pose: lodestone_model::EntityPose, scale: f32) -> (f32, f32, f32) {
    match pose {
        lodestone_model::EntityPose::Crouching => (0.6 * scale, 1.5 * scale, 1.27 * scale),
        lodestone_model::EntityPose::Swimming
        | lodestone_model::EntityPose::FallFlying
        | lodestone_model::EntityPose::SpinAttack => (0.6 * scale, 0.6 * scale, 0.4 * scale),
        lodestone_model::EntityPose::Sleeping => (0.2, 0.2, 0.2),
        lodestone_model::EntityPose::Dying => (0.2, 0.2, 1.62),
        lodestone_model::EntityPose::Standing
        | lodestone_model::EntityPose::LongJumping
        | lodestone_model::EntityPose::Sitting
        | lodestone_model::EntityPose::Other(_) => (0.6 * scale, 1.8 * scale, 1.62 * scale),
        _ => (0.6 * scale, 1.8 * scale, 1.62 * scale),
    }
}

/// F3+G: the borders of the chunk the player is standing in.
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
/// segments — comfortably inside the initial debug-line capacity alongside a
/// screen of hitboxes.
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

/// The two F3 sub-modes' own contribution to the debug-line channel, each
/// gated on its **own** flag.
///
/// A named function rather than two `if`s inlined in
/// `app::session::WindowApp::install_debug_lines_source`'s closure, because the
/// flag-to-producer mapping is the part of this feature that can cross without
/// leaving a trace: the two flags are adjacent `bool`s, and a fixture that sets
/// them to the same value cannot see a transposition at all. Pulling the mapping
/// out gives it a subject a gate can drive with the two flags at *different*
/// values, with no window, no GPU and no `World` —
/// `tests/debug_line_f3_overlay.rs`.
///
/// The caller resolves `draws` and `player` from the `World`; both are ignored
/// when the corresponding flag is off, so it is free to pass empty/dummy values
/// rather than pay for a read it will not use.
#[must_use]
pub fn f3_overlay_vertices(
    draws: &[crate::entities::EntityDraw],
    player: [f64; 3],
    min_y: i32,
    height: u32,
    hitboxes: bool,
    chunk_borders: bool,
) -> Vec<DebugLineVertex> {
    f3_overlay_vertices_with_states(
        draws,
        &[],
        player,
        min_y,
        height,
        hitboxes,
        chunk_borders,
    )
}

/// [`f3_overlay_vertices`] with the authoritative per-entity pose and scale
/// snapshots. Kept separate so narrow render tests can continue to exercise
/// the flag routing without manufacturing an ECS world; the live app calls
/// this form whenever F3+B is enabled.
#[must_use]
pub(crate) fn f3_overlay_vertices_with_states(
    draws: &[crate::entities::EntityDraw],
    states: &[EntityHitboxState],
    player: [f64; 3],
    min_y: i32,
    height: u32,
    hitboxes: bool,
    chunk_borders: bool,
) -> Vec<DebugLineVertex> {
    let mut out = Vec::new();
    if hitboxes {
        out.extend(entity_hitbox_vertices_with_states(draws, states));
    }
    if chunk_borders {
        out.extend(chunk_border_vertices(player, min_y, height));
    }
    out
}

/// Compatibility name for neighboring GPU docs that describe the original
/// fixed allocation. This is only the initial capacity now; the renderer grows
/// beyond it on demand.
pub(super) const MAX_DEBUG_LINE_SEGMENTS: usize = 4096;

/// Initial capacity for the debug-line pass, in line segments (two
/// [`DebugLineVertex`] inputs each — the wire shape callers still build). The
/// retained buffer grows geometrically when a frame needs more; it never
/// shrinks, so steady-state frames do not churn GPU allocations.
pub(super) const INITIAL_DEBUG_LINE_SEGMENTS: usize = MAX_DEBUG_LINE_SEGMENTS;

/// Vertices the GPU actually draws per input segment: two triangles forming a
/// screen-space-thickened quad — see [`DebugLineRenderer`]'s module doc for
/// why this replaced a `LineList` segment.
const VERTS_PER_SEGMENT: usize = 6;
/// Floats per ribbon vertex: `position.xyz`, `other.xyz` (the segment's other
/// endpoint, for the vertex shader's screen-space direction), `side`
/// (-1.0 / +1.0), `color.rgba`.
const FLOATS_PER_RIBBON_VERT: usize = 3 + 3 + 1 + 4;

/// Choose the next retained capacity without shrinking or growing one frame
/// at a time. `max_segments` is derived from the device's buffer limit.
fn grown_segment_capacity(current: usize, required: usize, max_segments: usize) -> usize {
    let max_segments = max_segments.max(current);
    let mut capacity = current.max(1).min(max_segments);
    while capacity < required && capacity < max_segments {
        capacity = capacity
            .saturating_mul(2)
            .max(capacity.saturating_add(1))
            .min(max_segments);
    }
    capacity
}

#[cfg(test)]
mod tests {
    use super::{grown_segment_capacity, player_hitbox_metrics};

    #[test]
    fn capacity_grows_geometrically_and_never_shrinks() {
        assert_eq!(grown_segment_capacity(4096, 4096, 32_768), 4096);
        assert_eq!(grown_segment_capacity(4096, 4097, 32_768), 8192);
        assert_eq!(grown_segment_capacity(8192, 5000, 32_768), 8192);
    }

    #[test]
    fn capacity_stops_at_device_limit() {
        assert_eq!(grown_segment_capacity(4096, 20_000, 10_000), 10_000);
    }

    #[test]
    fn player_pose_boxes_and_eyes_are_the_avatar_records_not_height_fractions() {
        use lodestone_model::EntityPose;

        assert_eq!(player_hitbox_metrics(EntityPose::Standing, 1.0), (0.6, 1.8, 1.62));
        assert_eq!(player_hitbox_metrics(EntityPose::Crouching, 1.0), (0.6, 1.5, 1.27));
        assert_eq!(player_hitbox_metrics(EntityPose::Swimming, 2.0), (1.2, 1.2, 0.8));
        assert_eq!(player_hitbox_metrics(EntityPose::Sleeping, 2.0), (0.2, 0.2, 0.2));
        assert_eq!(player_hitbox_metrics(EntityPose::Dying, 2.0), (0.2, 0.2, 1.62));
    }
}

/// Minimum on-screen width for F3 debug-line geometry (entity hitboxes, chunk
/// borders, and any plugin's [`DebugLinesSource`] segments), in logical
/// pixels, scaled the same way [`OutlineRenderer`]'s `MIN_LINE_WIDTH_PX` is
/// (`Window.getAppropriateLineWidth`'s `max(min, windowWidth / reference *
/// min)` shape) — thinner than the block-highlight box (`2.5`) because this
/// is a diagnostic wireframe meant to read as a *line*, not a highlighted
/// edge, but wide enough to survive the failure mode this pass used to have:
/// a `PrimitiveTopology::LineList` segment rasterizes at exactly one
/// **physical** pixel regardless of resolution or DPI scale, which is why
/// F3+B/F3+G read as "too thin" (and, at a real gameplay resolution, close
/// to invisible) even while the closure feeding them was producing correct
/// geometry the whole time — see `docs/debug-overlay.md`'s line-width note.
pub(super) const MIN_LINE_WIDTH_PX: f32 = 1.5;
pub(super) const LINE_WIDTH_REFERENCE_PX: f32 = 1920.0;

/// `Window.getAppropriateLineWidth`'s own minimum — `max(2.5F, getWidth() /
/// 1920.0F * 2.5F)` — for the passes that draw a line vanilla itself draws
/// through its own generic-line render type rather than a diagnostic wireframe.
///
/// The fishing line is one (`FishingHookRenderer` reads
/// `windowRenderState.appropriateLineWidth` verbatim), and so is the
/// block-highlight box, which is why [`OutlineRenderer`] carries the same
/// number under its own name. [`MIN_LINE_WIDTH_PX`] above is **not** this: it is
/// deliberately thinner so an F3 overlay reads as a diagnostic.
pub(super) const VANILLA_LINE_WIDTH_PX: f32 = 2.5;

/// Draws arbitrary coloured world-space line segments — a pathfinder's
/// planned route, a reachability probe, anything a plugin wants visible for
/// debugging (`CLAUDE.md`'s island rule: a subsystem with no way onto the
/// screen is undebuggable by construction).
///
/// A generalisation of [`OutlineRenderer`] immediately above: the same
/// `view_proj` + viewport uniform, screen-space-ribbon vertex shader and
/// triangle-list topology, but a per-vertex colour instead of a hardcoded
/// black, and an arbitrary segment count instead of one
/// hardcoded unit cube. [`DebugLineRenderer::prepare`] does the ribbon
/// expansion — one input [`DebugLineVertex`] pair becomes
/// [`VERTS_PER_SEGMENT`] output vertices — so every caller of
/// [`entity_hitbox_vertices`]/[`chunk_border_vertices`]/
/// [`debug_line_vertices`] is unaffected by this module's own internal wire
/// format. The vertex buffer is growable and retained between frames; this is
/// why the renderer can still be used through `RenderState`'s shared `&self`
/// render entry points.
#[derive(Debug)]
pub(super) struct DebugLineRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: RefCell<wgpu::Buffer>,
    capacity_segments: Cell<usize>,
    warned_at_device_limit: Cell<bool>,
}

impl DebugLineRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-debug-lines-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/debug_lines.wgsl").into()),
        });

        // Same bind-group-layout shape as `OutlineRenderer`: one uniform
        // (`view_proj` plus the viewport/half-width vec4), nothing else. A
        // dedicated pipeline entirely outside the model shader's four bind
        // groups, so this pass has no bearing on the 4-bind-group floor
        // `CLAUDE.md` warns about (`gpu.rs`'s own `BlockPipeline`/
        // `ModelPipeline` are untouched by this addition).
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

        // 64 bytes for view_proj + 16 bytes for the viewport/half-width vec4
        // — same layout as `OutlineRenderer`'s uniform.
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-debug-lines-uniform"),
            size: 80,
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
            size: (INITIAL_DEBUG_LINE_SEGMENTS * VERTS_PER_SEGMENT * FLOATS_PER_RIBBON_VERT
                * std::mem::size_of::<f32>()) as u64,
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
                    array_stride: (FLOATS_PER_RIBBON_VERT * std::mem::size_of::<f32>()) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: (3 * std::mem::size_of::<f32>()) as u64,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: (6 * std::mem::size_of::<f32>()) as u64,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: (7 * std::mem::size_of::<f32>()) as u64,
                            shader_location: 3,
                        },
                    ],
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
                topology: wgpu::PrimitiveTopology::TriangleList,
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
                depth_compare: Some(DEPTH_COMPARE_NEARER_OR_EQUAL),
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
            vertices: RefCell::new(vertices),
            capacity_segments: Cell::new(INITIAL_DEBUG_LINE_SEGMENTS),
            warned_at_device_limit: Cell::new(false),
        }
    }

    fn ensure_capacity(&self, device: &wgpu::Device, required_segments: usize) -> usize {
        let current = self.capacity_segments.get();
        if required_segments <= current {
            return required_segments;
        }

        let bytes_per_segment = VERTS_PER_SEGMENT
            .saturating_mul(FLOATS_PER_RIBBON_VERT)
            .saturating_mul(std::mem::size_of::<f32>());
        let max_segments = usize::try_from(device.limits().max_buffer_size)
            .unwrap_or(usize::MAX)
            / bytes_per_segment.max(1);
        let new_capacity = grown_segment_capacity(current, required_segments, max_segments);
        if new_capacity < required_segments && !self.warned_at_device_limit.replace(true) {
            tracing::warn!(
                target: "render",
                requested_segments = required_segments,
                retained_segments = new_capacity,
                "debug-line overlay reached the device vertex-buffer limit"
            );
        }
        if new_capacity == current {
            return current;
        }

        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-debug-lines-vertices-grown"),
            size: (new_capacity * bytes_per_segment) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *self.vertices.borrow_mut() = vertices;
        self.capacity_segments.set(new_capacity);
        new_capacity.min(required_segments)
    }

    /// Upload this frame's view-projection, the viewport/line-width uniform,
    /// and the ribbon-expanded line vertices. Must run before the render pass
    /// opens — buffers cannot be written mid-pass. `vertices` is the wire
    /// shape callers already build (flat, two [`DebugLineVertex`]s per
    /// segment); this expands each pair into [`VERTS_PER_SEGMENT`] on-screen
    /// ribbon vertices. The retained GPU buffer grows geometrically as needed
    /// and is bounded by the device's maximum vertex-buffer size.
    /// `viewport_px` is the render target's size in physical pixels and
    /// `min_width_px` the on-screen thickness floor, scaled by
    /// `Window.getAppropriateLineWidth`'s own `max(min, width / 1920 * min)`
    /// shape — see [`MIN_LINE_WIDTH_PX`]'s doc for why this pass is not a
    /// `LineList`, and [`VANILLA_LINE_WIDTH_PX`] for the other value callers
    /// pass.
    ///
    /// # Why the width is a parameter rather than the constant
    ///
    /// This renderer has **two** instances. The F3 overlay wants a thin
    /// diagnostic wireframe ([`MIN_LINE_WIDTH_PX`]); the fishing line is a piece
    /// of gameplay geometry vanilla draws at `appropriateLineWidth`
    /// ([`VANILLA_LINE_WIDTH_PX`]), and rendering it at the diagnostic width
    /// makes it read as a hairline that vanishes at distance. Everything else —
    /// the pipeline, the shader, the ribbon expansion below — is shared, which
    /// is the point: this expansion already exists twice in this module tree
    /// (here and in [`OutlineRenderer`]) and a third copy is how the two
    /// gradually stop agreeing.
    ///
    /// Returns the vertex count actually written — pass it to
    /// [`draw`](Self::draw).
    ///
    /// Takes `&self`, not `&mut self`, because the render graph is shared; the
    /// growable buffer is updated through interior mutability.
    pub(super) fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        viewport_px: (u32, u32),
        min_width_px: f32,
        vertices: &[DebugLineVertex],
    ) -> u32 {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));

        let width_px = (viewport_px.0.max(1) as f32 / LINE_WIDTH_REFERENCE_PX * min_width_px)
            .max(min_width_px);
        let viewport_uniform: [f32; 4] = [
            viewport_px.0.max(1) as f32,
            viewport_px.1.max(1) as f32,
            width_px * 0.5,
            0.0,
        ];
        queue.write_buffer(&self.uniform, 64, bytemuck::bytes_of(&viewport_uniform));

        // Whole segments only — a dangling odd vertex (should never happen;
        // every producer in this module emits pairs) contributes nothing
        // rather than reading past the slice.
        let segment_count = self.ensure_capacity(device, vertices.len() / 2);
        if segment_count == 0 {
            return 0;
        }

        let mut out = vec![0f32; segment_count * VERTS_PER_SEGMENT * FLOATS_PER_RIBBON_VERT];
        for s in 0..segment_count {
            let a = vertices[s * 2];
            let b = vertices[s * 2 + 1];
            // Two triangles covering the quad: (A-, A+, B-) and (A+, B+, B-),
            // where `X-`/`X+` mean endpoint `X` pushed to the two sides of
            // **one** screen-space normal shared by the whole segment.
            //
            // The stored `side` is not that sign directly. The shader derives
            // the normal from `screen(other) - screen(this)`, which points the
            // *opposite* way at B, so B's normal is `-n` and B's stored `side`
            // has to be negated to land on the same edge of the ribbon as A's:
            // `B-` is `(b, a, +1.0)` and `B+` is `(b, a, -1.0)`. Emitting the
            // same sign at both endpoints makes the two triangles pick
            // *opposite* diagonals of the quad, which is a bow-tie rather than
            // a ribbon: measured at a 6.4 px line width, the drawn segment was
            // 6 px at both endpoints and 3 px at its midpoint, and biased to
            // one side rather than centred on the line. See
            // `tests/debug_line_ribbon_width_pixels.rs`, which measures the
            // profile and fails on that taper.
            //
            // Both input vertices of a debug-line segment always carry the same
            // colour (every producer in this module sets it once per segment),
            // so using `a.color` for the whole ribbon is not an approximation.
            let quad: [([f32; 3], [f32; 3], f32); VERTS_PER_SEGMENT] = [
                (a.position, b.position, -1.0),
                (a.position, b.position, 1.0),
                (b.position, a.position, 1.0),
                (a.position, b.position, 1.0),
                (b.position, a.position, -1.0),
                (b.position, a.position, 1.0),
            ];
            let base = s * VERTS_PER_SEGMENT * FLOATS_PER_RIBBON_VERT;
            for (i, (pos, other, side)) in quad.into_iter().enumerate() {
                let v = base + i * FLOATS_PER_RIBBON_VERT;
                out[v..v + 3].copy_from_slice(&pos);
                out[v + 3..v + 6].copy_from_slice(&other);
                out[v + 6] = side;
                out[v + 7..v + 11].copy_from_slice(&a.color);
            }
        }
        queue.write_buffer(
            &self.vertices.borrow(),
            0,
            bytemuck::cast_slice(&out),
        );
        u32::try_from(segment_count * VERTS_PER_SEGMENT).unwrap_or(u32::MAX)
    }

    /// Record the draw. No-op when `vertex_count` (the last
    /// [`prepare`](Self::prepare)'s return value) is zero.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, vertex_count: u32) {
        if vertex_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        let vertices = self.vertices.borrow();
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.draw(0..vertex_count, 0..1);
    }
}

/// Polled source for this frame's world-space debug lines — the render half
/// of `ExtractSet::Debug` (`docs/plugin-api.md`). Same idiom as
/// [`OutlineShapeSource`]/[`ThirdPersonBodySource`] immediately below: the
/// renderer cannot reach the ECS `DebugLines` resource directly (this crate
/// has no dependency edge back to whoever owns the `World`), and threading it
/// through [`RenderState::render`]'s signature would touch every call site —
/// which, in practice, means `app.rs`'s `render(...)` calls, and
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
/// render_state.set_debug_lines_source(move |_| {
///     let world = ecs_handle.read();
///     lodestone_render_shell::gpu::debug_line_vertices(
///         &world.resource::<lodestone_ecs::player::DebugLines>().0,
///     )
/// });
/// ```
#[derive(Default)]
pub struct DebugLinesSource(
    #[allow(clippy::type_complexity)]
    pub(super) Option<Box<dyn Fn(glam::Vec3) -> Vec<DebugLineVertex> + Send + Sync>>,
);

impl DebugLinesSource {
    #[must_use]
    pub(super) fn sample(&self, eye: glam::Vec3) -> Vec<DebugLineVertex> {
        self.0.as_ref().map_or_else(Vec::new, |f| f(eye))
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
