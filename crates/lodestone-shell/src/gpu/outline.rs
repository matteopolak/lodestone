//! The mining-crack target descriptor and the block-outline wireframe pass.
use lodestone_data::block_states::StateId;
use lodestone_model::math::BlockPos;
use lodestone_render::{CAMERA_DEPTH_BIAS, DEPTH_COMPARE_NEARER_OR_EQUAL, DEPTH_FORMAT};

/// The block currently being mined, for the progressive crack overlay: its world
/// position, validated built-in state id (to resolve the block's real model
/// geometry) and destruction stage `0..=9`. Passed to
/// [`RenderState::render_with_crack`].
#[derive(Debug, Clone, Copy)]
pub struct CrackTarget {
    /// World block position of the target.
    pub block: [i32; 3],
    /// Validated built-in state id, used to resolve the block's baked quads.
    pub state_id: StateId,
    /// Destruction stage `0..=9`; selects the `destroy_stage_N` sprite.
    pub stage: u8,
}

/// Assembles the full per-frame `cracks` slice: the local
/// player's own dig, if any, plus one [`CrackTarget`] for every *other*
/// player's active overlay in `overlays` — the enumeration
/// `lodestone_game::mining::BlockDestructionOverlays::iter` exists for.
///
/// This is the actual gather logic, kept version/`Sim`-agnostic and pure so it
/// can be driven directly by a real
/// `BlockDestructionOverlays`/`ClientEvent::BlockDestruction` fold in a test —
/// not by hand-building a `Vec<CrackTarget>`, which
/// `crack_multi_target_pixels.rs` already covers and which cannot tell "the
/// gather is wired" from "the pipeline can draw N targets". `Sim`'s own
/// per-frame call is the thin, brokered wiring on top of this: resolve each
/// overlay's block position to a validated state id (`resolve` — live `NetClient::
/// block_at` or the offline demo world, mirroring `Sim::crack_target`'s own
/// resolution) and hand the result here.
///
/// A position `resolve` cannot answer (chunk not streamed in, e.g.) is
/// dropped rather than drawn with an unavailable state id — the same "no crack
/// without real geometry" rule `Sim::crack_target` already follows for the
/// local dig.
pub fn gather_crack_targets(
    local: Option<CrackTarget>,
    overlays: impl Iterator<Item = (BlockPos, u8)>,
    mut resolve: impl FnMut(BlockPos) -> Option<StateId>,
) -> Vec<CrackTarget> {
    let mut out: Vec<CrackTarget> = Vec::new();
    out.extend(local);
    for (pos, stage) in overlays {
        if stage >= 10 {
            // `BlockDestructionOverlays::set` already drops out-of-range
            // stages on write, so `iter()` should never yield one — this is
            // belt-and-suspenders against a future change to that invariant,
            // not a path this function relies on.
            continue;
        }
        if let Some(state_id) = resolve(pos) {
            out.push(CrackTarget {
                block: [pos.x, pos.y, pos.z],
                state_id,
                stage,
            });
        }
    }
    out
}

#[cfg(test)]
mod gather_tests {
    use super::*;
    use lodestone_game::mining::BlockDestructionOverlays;
    use lodestone_model::ClientEvent;

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    fn state(raw: u32) -> StateId {
        StateId::new(raw).expect("test state must be in the built-in census")
    }

    /// The gather must fold a real `BlockDestructionOverlays` — built through
    /// its real `ClientEvent::BlockDestruction` production path, not a
    /// hand-built `Vec<CrackTarget>` — into one `CrackTarget` per active
    /// entity, plus the local target when present.
    #[test]
    fn gathers_local_plus_every_other_players_overlay() {
        let mut overlays = BlockDestructionOverlays::new();
        let a = pos(1, 0, 3);
        let b = pos(-2, 0, 3);
        overlays.apply(&ClientEvent::BlockDestruction {
            entity_id: 11,
            pos: a,
            progress: 9,
        });
        overlays.apply(&ClientEvent::BlockDestruction {
            entity_id: 22,
            pos: b,
            progress: 5,
        });

        let local = CrackTarget {
            block: [0, 0, 0],
            state_id: state(42),
            stage: 3,
        };
        let mut resolved: Vec<BlockPos> = Vec::new();
        let targets = gather_crack_targets(Some(local), overlays.iter(), |p| {
            resolved.push(p);
            Some(state(100))
        });

        assert_eq!(targets.len(), 3, "local + two other-player overlays");
        assert!(
            targets
                .iter()
                .any(|t| t.block == [0, 0, 0] && t.stage == 3 && t.state_id == state(42)),
            "the local target must pass through unchanged"
        );
        assert!(
            targets
                .iter()
                .any(|t| t.block == [1, 0, 3] && t.stage == 9 && t.state_id == state(100)),
            "entity 11's overlay must resolve to a CrackTarget"
        );
        assert!(
            targets
                .iter()
                .any(|t| t.block == [-2, 0, 3] && t.stage == 5 && t.state_id == state(100)),
            "entity 22's overlay must resolve to a CrackTarget"
        );
        assert_eq!(resolved.len(), 2, "resolve is called once per overlay, not per local target");
    }

    /// A position `resolve` cannot answer is dropped, not drawn with a bogus
    /// state id — proves the gather does not silently fabricate geometry for
    /// an unstreamed chunk.
    #[test]
    fn unresolvable_overlay_is_dropped_not_faked() {
        let mut overlays = BlockDestructionOverlays::new();
        overlays.apply(&ClientEvent::BlockDestruction {
            entity_id: 1,
            pos: pos(0, 0, 0),
            progress: 4,
        });
        let targets = gather_crack_targets(None, overlays.iter(), |_| None);
        assert!(
            targets.is_empty(),
            "an overlay `resolve` cannot answer must not appear in the output"
        );
    }

    /// No local target and no overlays gathers nothing — the empty-slice
    /// no-op path `crack_multi_target_pixels.rs` already proves is cheap.
    #[test]
    fn empty_input_gathers_nothing() {
        let overlays = BlockDestructionOverlays::new();
        let targets = gather_crack_targets(None, overlays.iter(), |_| Some(state(1)));
        assert!(targets.is_empty());
    }
}

/// The 12 edges of a unit cube as pairs of corner indices.
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

/// Vertices per edge: two triangles (6 verts) forming a screen-space-thickened
/// quad — see the module doc on [`OutlineRenderer`] for why a quad rather than
/// a `LineList` primitive.
const VERTS_PER_EDGE: usize = 6;
/// Floats per vertex: `position.xyz`, `other.xyz` (the edge's other endpoint,
/// used by the vertex shader to find the screen-space line direction), `side`
/// (-1.0 / +1.0, which way this vertex is pushed off the line's centre).
const FLOATS_PER_VERT: usize = 7;

/// Minimum vanilla-style line width in logical pixels, and the reference
/// window width it scales from. Ported from vanilla's own
/// appropriate-line-width calculation:
/// `max(2.5, windowWidth / 1920 * 2.5)`. That is the width the *real* hit
/// outline draws with — see [`OutlineRenderer`]'s doc for why this is not the
/// F3 debug-shape call site.
const MIN_LINE_WIDTH_PX: f32 = 2.5;
const LINE_WIDTH_REFERENCE_PX: f32 = 1920.0;

/// Draws a black wireframe box around the targeted block. Its own pipeline
/// (screen-space-thickened triangle geometry, `LessEqual` depth, no depth
/// write, alpha-blended) so it reads clearly over terrain without a second
/// pass or z-fighting.
///
/// ## Why triangles, not `LineList`
///
/// An earlier version of this pass drew the 12 edges as `PrimitiveTopology::LineList`,
/// which rasterizes at exactly one *physical* pixel regardless of resolution or
/// DPI scale. Vanilla's real hit-outline draw — its own submit-block-outline
/// path's non-debug submit-hit-outline branch (**not**
/// the F3-style collision/occlusion/interaction shape dump at `:740-758`, which
/// is gated behind `SharedConstants.DEBUG_SHAPES` and is a different draw
/// entirely) — passes an explicit `width` argument down to
/// its own submit-shape-outline call, sourced from
/// vanilla's own game-render-state's window-render-state's appropriate-line-width
///. That width is attached per-vertex via
/// vanilla's own vertex-consumer set-line-width call and
/// expanded into real screen-space quad geometry downstream, because — same
/// conclusion the issue reached — wgpu (and modern Minecraft's own renderer,
/// for the same reason) does not portably support a GPU line-width parameter.
///
/// So: each edge here is submitted as a quad (`VERTS_PER_EDGE` vertices, two
/// triangles) rather than a single `LineList` segment. The vertex shader
/// carries both endpoints of the edge, transforms them to screen space,
/// derives the on-screen perpendicular direction, and pushes each vertex out
/// by half the configured pixel width along it — the same "line as a
/// screen-space ribbon" technique vanilla's `setLineWidth` path performs, just
/// expanded on our side rather than in a downstream vertex-format consumer.
/// The distance travelled is computed in real device pixels (via the
/// `viewport` uniform), so it holds constant size on screen at any DPI scale,
/// unlike the old 1-physical-pixel `LineList` line.
///
/// The colour/alpha path was **not** the bug: vanilla's real (non-debug) hit
/// outline draws at vanilla's own opaque-black-with-alpha helper applied to `102` — alpha ≈ 0.4 — while this pass already
/// used 0.6, so ours was already the more opaque of the two. That is left
/// unchanged; only the geometry generation changed.
///
/// The depth setup was also checked and left alone: vanilla's `LINES` render
/// pipeline uses `DepthStencilState.DEFAULT`
/// (`GREATER_THAN_OR_EQUAL`, **no** bias) — the `LINES_DEPTH_BIAS` variant at
/// `:572` exists but is not what the hit outline uses. Per `CLAUDE.md`,
/// vanilla's `GREATER_THAN_OR_EQUAL` under reversed-Z is this engine's
/// `LessEqual` under `[0,1]` depth, which is exactly what this pipeline
/// already had, at zero bias — so there was no sign-flipped bias to fix.
/// The box stays on the selected shape's *exact* boundary. The earlier
/// world-space expansion made the screen-space ribbon appear to breathe as an
/// oblique camera moved: its projected separation changed with the view even
/// though the ribbon width was constant. The inclusive compare plus the
/// camera-facing depth bias resolves the depth tie without moving the line in
/// world space.
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
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/outline.wgsl").into()),
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

        // 64 bytes for view_proj + 16 bytes for the viewport/half-width vec4.
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-uniform"),
            size: 80,
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

        // 12 edges × VERTS_PER_EDGE vertices, FLOATS_PER_VERT f32 each.
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-outline-vertices"),
            size: (12 * VERTS_PER_EDGE * FLOATS_PER_VERT * std::mem::size_of::<f32>()) as u64,
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
                    array_stride: (FLOATS_PER_VERT * std::mem::size_of::<f32>()) as u64,
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
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(DEPTH_COMPARE_NEARER_OR_EQUAL),
                stencil: wgpu::StencilState::default(),
                bias: CAMERA_DEPTH_BIAS,
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

    /// Upload the view-projection, viewport/line-width uniform, and the exact
    /// outline-shape vertices for `block`. Must be called before the render pass begins —
    /// buffers can't be written mid-pass. `outline` is the block's real
    /// outline shape in world space, from [`LiveCollision::outline_boxes_at`].
    /// Pass an empty slice to fall back to a unit cube — correct for the demo
    /// palette, which has no outline census and is all full cubes anyway.
    /// `viewport_px` is the render target's size in physical pixels, used to
    /// size the on-screen line thickness (see the module doc's
    /// `MIN_LINE_WIDTH_PX` citation).
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
    /// and needs no change to the edge geometry below.
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        block: [i32; 3],
        outline: &[lodestone_physics::Aabb],
        viewport_px: (u32, u32),
    ) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));

        let width_px = (viewport_px.0.max(1) as f32 / LINE_WIDTH_REFERENCE_PX
            * MIN_LINE_WIDTH_PX)
            .max(MIN_LINE_WIDTH_PX);
        let viewport_uniform: [f32; 4] = [
            viewport_px.0.max(1) as f32,
            viewport_px.1.max(1) as f32,
            width_px * 0.5,
            0.0,
        ];
        queue.write_buffer(&self.uniform, 64, bytemuck::bytes_of(&viewport_uniform));

        let (mut lo, mut hi) = (
            [
                block[0] as f32,
                block[1] as f32,
                block[2] as f32,
            ],
            [
                block[0] as f32 + 1.0,
                block[1] as f32 + 1.0,
                block[2] as f32 + 1.0,
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
                b.min_x as f32,
                b.min_y as f32,
                b.min_z as f32,
            ];
            hi = [
                b.max_x as f32,
                b.max_y as f32,
                b.max_z as f32,
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
        let mut verts = [0f32; 12 * VERTS_PER_EDGE * FLOATS_PER_VERT];
        for (e, &(a, b)) in CUBE_EDGES.iter().enumerate() {
            let ca = corner(a);
            let cb = corner(b);
            // Two triangles covering the quad: (A-, A+, B-) and (A+, B+, B-),
            // where `A-`/`A+` are endpoint A pushed to the ribbon's minus/plus
            // edge and likewise for B.
            //
            // **The stored `side` is negated for every B vertex, and must be.**
            // The shader offsets along `perp(screen(other) - screen(this))`, so
            // at A that vector is B-A and at B it is A-B -- the normal points
            // the opposite way at the far endpoint. A B vertex therefore needs
            // the *opposite* stored side to land on the same edge as the A
            // vertex it pairs with. Storing the edge's own sign at both ends
            // makes the two triangles pick opposite diagonals: a bow-tie that
            // pinches to zero width at the segment's midpoint while staying
            // full width at both endpoints, which reads as "the line is thin
            // and uneven" rather than as a wrong quad. The debug-line pass had
            // the identical defect; see `gpu/debug_lines.rs` and
            // `docs/debug-overlay.md` for the measured width profile.
            let quad: [([f32; 3], [f32; 3], f32); VERTS_PER_EDGE] = [
                (ca, cb, -1.0),
                (ca, cb, 1.0),
                (cb, ca, 1.0),
                (ca, cb, 1.0),
                (cb, ca, -1.0),
                (cb, ca, 1.0),
            ];
            let base = e * VERTS_PER_EDGE * FLOATS_PER_VERT;
            for (i, (pos, other, side)) in quad.into_iter().enumerate() {
                let v = base + i * FLOATS_PER_VERT;
                verts[v..v + 3].copy_from_slice(&pos);
                verts[v + 3..v + 6].copy_from_slice(&other);
                verts[v + 6] = side;
            }
        }
        queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&verts));
    }

    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..(12 * VERTS_PER_EDGE) as u32, 0..1);
    }
}
