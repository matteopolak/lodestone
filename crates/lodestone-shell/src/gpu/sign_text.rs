//! World-space sign text (that fix's sign scope): coloured quads placed by
//! [`lodestone_render::sign_text_transform`], reusing `gpu/nametag.rs`'s
//! jar-sourced font loader and ink-run layout rather than reinventing either
//! — [`super::nametag::layout_ink_runs`]/[`super::nametag::load_font`] are
//! `pub(super)` for exactly this, since both files are submodules of
//! `crate::gpu`. The shader is the identical file too
//! (`shaders/nametag.wgsl`: one `view_proj` uniform, a flat vertex colour, no
//! texture) — a coloured-quad-from-a-raster-font pass is the same shape here
//! as it is there, so a second `.wgsl` would be a copy with nothing to
//! diverge.
//!
//! # Depth: vanilla's `TEXT_POLYGON_OFFSET` pipeline, ported
//!
//! `AbstractSignRenderer.submitSignText` submits with
//! `Font.DisplayMode.POLYGON_OFFSET`, which resolves to
//! `RenderPipelines.TEXT_POLYGON_OFFSET`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/RenderPipelines.java`):
//! `new DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, true, 1.0F, 10.0F)`.
//! `crack_pipeline.rs` already worked out this record's real field order from
//! `VulkanRenderPipeline.java` — `(depthTest, writeDepth,
//! depthBiasScaleFactor, depthBiasConstant)` — for the *same* two numeric
//! constants (`1.0F`, `10.0F`) on a different pipeline
//! (`GREATER_THAN_OR_EQUAL, false, 1.0F, 10.0F`, vanilla's `pipeline/
//! crumbling`). Only `writeDepth` differs between the two: sign text is
//! `true` (a nearer line of text must occlude a farther one, and the board
//! itself, the way any opaque geometry does), the crack overlay is `false`
//! (it is a decal and must never occlude anything). Vanilla is reversed-Z;
//! this project's depth is `[0,1]` DirectX-style
//! (`CLAUDE.md`'s rendering constraints), so `GREATER_THAN_OR_EQUAL` flips to
//! [`wgpu::CompareFunction::LessEqual`] and the bias flips sign —
//! `constant: -10, slope_scale: -1.0`, identical to `crack_pipeline.rs`'s
//! port of the same two Java constants, pulling the text quads toward the
//! camera just enough to win the depth test against the coplanar board
//! without z-fighting.
//!
//! # Not a billboard
//!
//! Unlike [`super::nametag`], sign text has a fixed world orientation baked
//! into [`lodestone_render::sign_text_transform`] (the sign's own facing or
//! rotation), not a camera-facing basis — so there is no `right`/`up`
//! argument and no per-frame camera dependency beyond the shared `view_proj`
//! uniform every world-space pass in this crate already writes.
//!
//! # What is deliberately not built
//!
//! See `lodestone_render::sign` module doc's "Deferred" section for the two
//! real gaps this pass inherits (the black-dye-glowing outline, and
//! per-glyph world-light modulation) — both documented there rather than
//! here because they are properties of the *colour this pass is handed*, not
//! of how this pass draws it.

use glam::Vec3;
use lodestone_assets::font::RasterFont;
use lodestone_render::{
    DEPTH_FORMAT, SignKind, SignOrientation, SignSpawn, sign_side_color, sign_text_transform,
};
use lodestone_world::SignSide;

/// Same vertex shape as `gpu/nametag.rs`'s own `NameTagVertex` — kept as its
/// own type rather than shared, matching this crate's established
/// one-vertex-type-per-pass pattern (see `gpu/outline.rs`/
/// `gpu/debug_lines.rs`).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SignTextVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// Fixed vertex capacity, same fixed-buffer idiom as
/// [`super::nametag::MAX_NAME_TAG_VERTICES`]. A sign carries at most 8 lines
/// (4 per side) of at most `MAX_TEXT_LINE_WIDTH` pixels each; this
/// comfortably covers a screen full of signed boards.
const MAX_SIGN_TEXT_VERTICES: usize = 40_000;

/// Draws world-space sign text — see the module doc for the depth pipeline
/// and why this is not a billboard.
#[derive(Debug)]
pub(super) struct SignTextRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
    /// `None` off a jar-less run — same fail-open contract as
    /// [`super::nametag::NameTagRenderer::font`].
    font: Option<RasterFont>,
    /// Ink-run layouts, persisted across frames — sign text
    /// changes only on a block-entity update, so the texel walk must not run
    /// per frame either. Shares [`super::nametag::InkLayoutCache`] for the same
    /// reason this file already shares `layout_ink_runs`.
    ink: super::nametag::InkLayoutCache,
}

impl SignTextRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-sign-text-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/nametag.wgsl").into()),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-sign-text-bgl"),
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
            label: Some("lodestone-sign-text-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-sign-text-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-sign-text-vertices"),
            size: (MAX_SIGN_TEXT_VERTICES * std::mem::size_of::<SignTextVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-sign-text-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<SignTextVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        })];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-sign-text-pipeline"),
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // No culling, matching `gpu/nametag.rs`: the two sides of a
                // sign already draw independently through their own
                // transform, so there is no back face for either quad to be.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                // `writeDepth = true` in vanilla's own
                // `TEXT_POLYGON_OFFSET` record — see the module doc.
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: -10,
                    slope_scale: -1.0,
                    clamp: 0.0,
                },
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
            font: super::nametag::load_font(),
            ink: super::nametag::InkLayoutCache::default(),
        }
    }

    /// Uploads this frame's view-projection and sign-text vertices. Must run
    /// before the render pass opens, same buffer-creation constraint as
    /// every other pass in this file. Returns the vertex count, capped at
    /// [`MAX_SIGN_TEXT_VERTICES`] — pass to [`draw`](Self::draw).
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        signs: &[SignSpawn],
    ) -> u32 {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));
        let Some(raster) = &self.font else {
            return 0;
        };

        let mut vertices = Vec::new();
        for spawn in signs {
            push_side_quads(
                raster,
                &self.ink,
                &spawn.front,
                spawn.pos,
                spawn.kind,
                spawn.orientation,
                true,
                &mut vertices,
            );
            push_side_quads(
                raster,
                &self.ink,
                &spawn.back,
                spawn.pos,
                spawn.kind,
                spawn.orientation,
                false,
                &mut vertices,
            );
        }
        let len = vertices.len().min(MAX_SIGN_TEXT_VERTICES);
        if len > 0 {
            queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&vertices[..len]));
        }
        len as u32
    }

    /// Records the draw (no-op with zero vertices, including the no-jar
    /// `font: None` state, since [`prepare`](Self::prepare) always returns
    /// `0` there).
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.draw(0..count, 0..1);
    }
}

/// Lowers one text side into world-space quads, appended onto `out`. A no-op
/// for a side whose four lines are all empty — vanilla's own font-split of
/// an empty string contributes no ink either, so this is an optimisation,
/// not a behaviour change.
#[allow(clippy::too_many_arguments)]
fn push_side_quads(
    raster: &RasterFont,
    ink: &super::nametag::InkLayoutCache,
    side: &SignSide,
    pos: [i32; 3],
    kind: SignKind,
    orientation: SignOrientation,
    is_front: bool,
    out: &mut Vec<SignTextVertex>,
) {
    if side.lines.iter().all(String::is_empty) {
        return;
    }
    let matrix = sign_text_transform(pos, kind, orientation, is_front);
    let color = sign_side_color(side);
    // `AbstractSignRenderer.submitSignText`: `signMidpoint = 4 *
    // textLineHeight / 2`, i.e. two full lines — line `i`'s top sits at
    // `i * textLineHeight - signMidpoint`. The height is the **block
    // entity's**, not a constant: a hanging sign overrides it to 9.
    let line_height = kind.text_line_height();
    let sign_midpoint = 2.0 * line_height;
    for (i, line) in side.lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let layout = ink.layout(raster, line);
        let (rects, total_width) = (&layout.0, layout.1);
        // Each line is centred independently, matching `x1 =
        // -font.width(line) / 2` — not all four lines sharing one width.
        let x1 = -total_width / 2.0;
        let y_off = i as f32 * line_height - sign_midpoint;
        for rect in rects {
            let lx = rect.x + x1;
            let ly = rect.y + y_off;
            // Fed **unflipped** into the placement matrix: its own `-Y`
            // scale carries the pixel-space-down to world-space-up flip —
            // see `lodestone_render::sign`'s module doc.
            let tl = matrix.transform_point3(Vec3::new(lx, ly, 0.0)).to_array();
            let tr = matrix
                .transform_point3(Vec3::new(lx + rect.w, ly, 0.0))
                .to_array();
            let bl = matrix
                .transform_point3(Vec3::new(lx, ly + rect.h, 0.0))
                .to_array();
            let br = matrix
                .transform_point3(Vec3::new(lx + rect.w, ly + rect.h, 0.0))
                .to_array();
            out.extend([
                SignTextVertex { position: tl, color },
                SignTextVertex { position: bl, color },
                SignTextVertex { position: tr, color },
                SignTextVertex { position: tr, color },
                SignTextVertex { position: bl, color },
                SignTextVertex { position: br, color },
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign_with_front_text(text: &str) -> SignSpawn {
        let mut spawn = SignSpawn::at([0, 0, 0]);
        spawn.front.lines[0] = text.to_owned();
        spawn
    }

    /// A blank sign (every line on both sides empty, `SignSpawn::at`'s own
    /// default) must contribute nothing — mirrors
    /// `gpu/nametag.rs`'s `an_empty_name_contributes_no_vertices`.
    #[test]
    fn a_blank_sign_contributes_no_vertices() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::InkLayoutCache::default();
        let mut out = Vec::new();
        let spawn = SignSpawn::at([0, 0, 0]);
        push_side_quads(
            &raster,
            &ink,
            &spawn.front,
            spawn.pos,
            spawn.kind,
            spawn.orientation,
            true,
            &mut out,
        );
        push_side_quads(
            &raster,
            &ink,
            &spawn.back,
            spawn.pos,
            spawn.kind,
            spawn.orientation,
            false,
            &mut out,
        );
        assert!(out.is_empty());
    }

    /// The positive control: real text on the front side contributes ink,
    /// and it contributes none to the back side, which was never given any
    /// text.
    #[test]
    fn text_on_one_side_only_contributes_to_that_side() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::InkLayoutCache::default();
        let spawn = sign_with_front_text("LODESTONE");
        let mut front = Vec::new();
        push_side_quads(
            &raster,
            &ink,
            &spawn.front,
            spawn.pos,
            spawn.kind,
            spawn.orientation,
            true,
            &mut front,
        );
        assert!(!front.is_empty(), "front text must contribute vertices");

        let mut back = Vec::new();
        push_side_quads(
            &raster,
            &ink,
            &spawn.back,
            spawn.pos,
            spawn.kind,
            spawn.orientation,
            false,
            &mut back,
        );
        assert!(back.is_empty(), "an untouched back side must contribute nothing");
    }
}
