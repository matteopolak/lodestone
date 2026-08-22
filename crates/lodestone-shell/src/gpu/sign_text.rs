//! World-space sign text: coloured quads placed by
//! [`lodestone_render::sign_text_transform`], reusing `gpu/nametag.rs`'s
//! jar-sourced font loader and *styled* ink-run layout rather than
//! reinventing either — [`super::nametag::layout_styled_ink_runs`]/
//! [`super::nametag::load_font`] are `pub(super)` for exactly this, since
//! both files are submodules of `crate::gpu`. The shader is the identical
//! file too (`shaders/nametag.wgsl`: one `view_proj` uniform, a flat vertex
//! colour, no texture) — a coloured-quad-from-a-raster-font pass is the same
//! shape here as it is there, so a second `.wgsl` would be a copy with
//! nothing to diverge.
//!
//! # Per-run styling, and the dye colour as a *default*
//!
//! [`lodestone_world::SignSide::lines`] carries real
//! [`lodestone_world::SignTextSpan`]s now (colour — including hex — bold,
//! italic, underline, strikethrough, inherited through nested JSON `extra`
//! components at parse time; see that type's module doc for why it is not
//! literally `lodestone_model::text::Text`). [`push_side_quads`] converts
//! each line's spans into real `lodestone_model::text::TextSpan`s and hands
//! them to [`super::nametag::layout_styled_ink_runs`] — the same styled
//! world-space glyph layout nametags and `text_display` already use, so
//! there is exactly one implementation of "turn styled spans into ink",
//! never a second.
//!
//! **The side's own dye colour is the default a run falls back to, not an
//! override**: `AbstractSignRenderer.submitSignText` passes the side's
//! resolved colour (full dye when glowing, `ARGB.scaleRGB(dye, 0.4)`
//! otherwise) as `Font`'s own default-colour argument, and
//! `Font.java::getTextColor` only substitutes it when a glyph's own `Style`
//! carries no colour at all — a run that *does* specify one always wins,
//! at any brightness. [`default_run_color`] resolves that default (via
//! [`sign_side_color`], the existing glow/dark-scale logic, unchanged), and
//! [`styled_spans`] fills it in only for runs whose own
//! [`lodestone_world::SignTextSpan::color`] is `None`.
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
//! of how this pass draws it. Neither is affected by per-run styling: an
//! explicit run colour is used verbatim, the same way the pre-existing dye
//! default was, so this change closes the "no per-run formatting" gap
//! without touching either of those two.

use glam::Vec3;
use lodestone_assets::font::RasterFont;
use lodestone_model::text::{TextColor, TextSpan, TextStyle};
use lodestone_render::{
    DEPTH_FORMAT, SignKind, SignOrientation, SignSpawn, sign_side_color, sign_text_transform,
};
use lodestone_world::{SignSide, SignTextSpan};

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
/// [`super::nametag::MAX_NAME_TAG_VERTICES`] — `prepare` takes `&self` and
/// is called from `RenderState::render`, which also takes `&self` and has no
/// device to hand, so this pass cannot reallocate the way a `&mut self` one
/// could.
///
/// **The old value was 40,000 and a real base exceeded it.** Measured with
/// the jar's own font through [`push_side_quads`]: the ink walk emits one
/// quad per *horizontal run of ink texels per glyph row*, not one per glyph,
/// so a plain sign with four full lines on **both** sides is **7,632**
/// vertices and one with three short lines on one side is **1,392**. At
/// 40,000 that is **5.2** fully-written signs, or ~28 ordinary ones — a
/// storage room's worth of labels. Past it [`prepare`](SignTextRenderer::
/// prepare) simply dropped the tail of a list sorted by **block position**,
/// so as the player moved and the in-range set changed, whole signs blinked
/// completely in and out with nothing logged anywhere.
///
/// 262,144 is 7.34 MB at 28 bytes a vertex — against ~67 MB of terrain mesh
/// at render distance 8, a real but proportionate cost — and buys 34
/// fully-written signs or ~188 ordinary ones **in front of the camera**
/// within the 64-block gather. It is still a budget rather than a guarantee,
/// which is why [`prepare`](SignTextRenderer::prepare) now spends it
/// nearest-first and says out loud when it binds instead of dropping in
/// silence.
const MAX_SIGN_TEXT_VERTICES: usize = 262_144;

/// How far behind the eye plane a sign's centre may sit and still be built.
/// A sign is under a block across, so one block of slack keeps a board the
/// camera is standing inside while discarding the half of the 64-block
/// gather that is strictly behind the player and can contribute no pixel.
/// This is a **budget** filter, not a visibility cull: everything it drops
/// projects to nothing anyway.
const BEHIND_EYE_SLACK: f32 = 1.0;

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
    /// Styled ink-run layouts, persisted across frames — sign text
    /// changes only on a block-entity update, so the texel walk must not run
    /// per frame either. Shares [`super::nametag::StyledInkLayoutCache`] for
    /// the same reason this file already shares `layout_styled_ink_runs`.
    ink: super::nametag::StyledInkLayoutCache,
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
            ink: super::nametag::StyledInkLayoutCache::default(),
        }
    }

    /// Uploads this frame's view-projection and sign-text vertices. Must run
    /// before the render pass opens, same buffer-creation constraint as
    /// every other pass in this file. Returns the vertex count — pass to
    /// [`draw`](Self::draw).
    ///
    /// # Which signs get the budget, and why the order is not the caller's
    ///
    /// `block_entities::sign_spawns` sorts its list by **block position**,
    /// deliberately, so pixel gates get a deterministic batch order. That is
    /// the wrong order to *spend a budget* in: it has nothing to do with the
    /// camera, so when the list overflowed [`MAX_SIGN_TEXT_VERTICES`] the
    /// signs that vanished were whichever ones happened to sort last, and
    /// they changed as the player moved and the in-range set changed. Whole
    /// boards blinked completely in and out.
    ///
    /// So this reorders by **forward distance** — `clip.w`, which for a
    /// perspective projection is exactly `-z_view` — nearest first, and
    /// discards anything more than [`BEHIND_EYE_SLACK`] behind the eye
    /// plane, which projects to nothing and was previously eating roughly
    /// half the budget. The caller's sort is untouched; this one is local to
    /// the upload.
    ///
    /// Truncation is at **whole-sign** granularity. The old `.min()` cut a
    /// flat vertex vector at an arbitrary index, which could sever a
    /// triangle and left the boundary sign drawing a fragment of its own
    /// text; and it was completely silent, which `CLAUDE.md`'s "nothing may
    /// be silently skipped" forbids. [`crate::sign_diagnostics::
    /// report_draw_budget`] now names the drop.
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

        let ordered = order_by_forward_distance(view_proj, signs);
        let mut vertices = Vec::new();
        let mut drawn = 0usize;
        for spawn in &ordered {
            let committed = vertices.len();
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
            if vertices.len() > MAX_SIGN_TEXT_VERTICES {
                // Sorted nearest-first, so everything left is farther than
                // this one: stop rather than keep probing for a smaller sign
                // that happens to fit, which would make the drawn set depend
                // on text length as well as distance.
                vertices.truncate(committed);
                break;
            }
            drawn += 1;
        }
        crate::sign_diagnostics::report_draw_budget(
            signs.len(),
            ordered.len(),
            drawn,
            vertices.len(),
            MAX_SIGN_TEXT_VERTICES,
        );
        if !vertices.is_empty() {
            queue.write_buffer(&self.vertices, 0, bytemuck::cast_slice(&vertices));
        }
        vertices.len() as u32
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

/// This frame's signs, nearest first, with everything strictly behind the eye
/// discarded — see [`SignTextRenderer::prepare`]'s doc for why the caller's
/// position sort is the wrong order to spend a fixed budget in.
///
/// `clip.w` for a perspective projection is `-z_view`, the distance along the
/// view axis, so this needs no camera basis and no eye position: the
/// view-projection the pass already receives carries both.
fn order_by_forward_distance<'a>(
    view_proj: &[[f32; 4]; 4],
    signs: &'a [SignSpawn],
) -> Vec<&'a SignSpawn> {
    let vp = glam::Mat4::from_cols_array_2d(view_proj);
    let mut ordered: Vec<(f32, &SignSpawn)> = signs
        .iter()
        .filter_map(|spawn| {
            let centre = glam::Vec4::new(
                spawn.pos[0] as f32 + 0.5,
                spawn.pos[1] as f32 + 0.5,
                spawn.pos[2] as f32 + 0.5,
                1.0,
            );
            let w = (vp * centre).w;
            (w > -BEHIND_EYE_SLACK).then_some((w, spawn))
        })
        .collect();
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
    ordered.into_iter().map(|(_, spawn)| spawn).collect()
}

/// This side's resolved default run colour, packed `0x00rrggbb` — the exact
/// value [`sign_side_color`] already computes (full dye when glowing,
/// `ARGB.scaleRGB(dye, 0.4)` otherwise), just converted back from its `0..=1`
/// float form into the integer form [`lodestone_world::SignTextSpan::color`]
/// and [`lodestone_model::text::TextColor::Rgb`] both use. A pure unit
/// conversion of an already-correct value, not a re-derivation of the
/// vanilla dark-scale formula (which stays the crate that already owns it) —
/// exact for every `0..=255` channel value `scale_rgb`'s own integer
/// truncation can produce.
fn default_run_color(side: &SignSide) -> u32 {
    let [r, g, b, _] = sign_side_color(side);
    let ch = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    (ch(r) << 16) | (ch(g) << 8) | ch(b)
}

/// Converts one line's already-flattened, already-inherited
/// [`SignTextSpan`]s into real `lodestone_model::text::TextSpan`s, filling
/// `default_rgb` in for any run whose own colour is `None` — see the module
/// doc's "Per-run styling, and the dye colour as a *default*" section for
/// why this is a fill-in-the-default rather than a flat override.
fn styled_spans(line: &[SignTextSpan], default_rgb: u32) -> Vec<TextSpan> {
    line.iter()
        .map(|span| TextSpan {
            text: span.text.clone(),
            style: TextStyle {
                color: Some(TextColor::Rgb(span.color.unwrap_or(default_rgb))),
                bold: Some(span.bold),
                italic: Some(span.italic),
                underlined: Some(span.underlined),
                strikethrough: Some(span.strikethrough),
                obfuscated: None,
                font: None,
            },
        })
        .collect()
}

/// Lowers one text side into world-space quads, appended onto `out`. A no-op
/// for a side whose four lines are all empty — vanilla's own font-split of
/// an empty string contributes no ink either, so this is an optimisation,
/// not a behaviour change.
#[allow(clippy::too_many_arguments)]
fn push_side_quads(
    raster: &RasterFont,
    ink: &super::nametag::StyledInkLayoutCache,
    side: &SignSide,
    pos: [i32; 3],
    kind: SignKind,
    orientation: SignOrientation,
    is_front: bool,
    out: &mut Vec<SignTextVertex>,
) {
    if side.lines.iter().all(Vec::is_empty) {
        return;
    }
    let matrix = sign_text_transform(pos, kind, orientation, is_front);
    let default_rgb = default_run_color(side);
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
        let spans = styled_spans(line, default_rgb);
        let layout = ink.layout(raster, &spans);
        let (rects, total_width) = (&layout.0, layout.1);
        // Each line is centred independently, matching `x1 =
        // -font.width(line) / 2` — not all four lines sharing one width.
        // `total_width` already accounts for bold's widened advance (see
        // `layout_styled_ink_runs`'s doc), so a bold line still centres
        // correctly against its own (wider) measured width.
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
            let color = rect.color;
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
    use lodestone_render::Camera;
    use lodestone_world::SignDyeColor;

    use super::*;

    fn plain_span(text: &str) -> SignTextSpan {
        SignTextSpan {
            text: text.to_owned(),
            ..Default::default()
        }
    }

    fn sign_with_front_text(text: &str) -> SignSpawn {
        let mut spawn = SignSpawn::at([0, 0, 0]);
        spawn.front.lines[0] = vec![plain_span(text)];
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
        let ink = super::super::nametag::StyledInkLayoutCache::default();
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
        let ink = super::super::nametag::StyledInkLayoutCache::default();
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


    /// **The magnitude gate for [`MAX_SIGN_TEXT_VERTICES`].** The owner
    /// reported signs "flicker in and out (completely) when they're far away
    /// and I move", and the pass's fixed vertex buffer is one of the two
    /// mechanisms in the sign path that can make a *whole* board's text
    /// vanish and return with camera motion (the other, a depth tie against
    /// the sign's own board, is measured and excluded by
    /// `tests/sign_text_distance_stability_pixels.rs`).
    ///
    /// This is deliberately a **prediction**, not a direction: it measures
    /// the real vertex cost of two real signs through the real
    /// [`push_side_quads`] and the jar's own font, then evaluates the
    /// capacity question against **both** hypotheses — the old 40,000 and
    /// whatever [`MAX_SIGN_TEXT_VERTICES`] is now — so the run has to land on
    /// one. Asserting only "the capacity is large enough" would pass at any
    /// value that happened to exceed the fixture.
    ///
    /// The population it demands is not a round number picked to be
    /// comfortable: it is what a storage room looks like. `FULL_SIGNS` is a
    /// four-line, two-sided sign, the most a `SignBlockEntity` can carry;
    /// `LABEL_SIGNS` is a one-line chest label, which is what people
    /// actually place dozens of.
    #[test]
    fn the_vertex_budget_holds_a_real_rooms_worth_of_signs() {
        /// What the cap was when the flicker was reported.
        const PREVIOUS_CAPACITY: usize = 40_000;
        /// Four full lines both sides, the most one sign can carry.
        const FULL_SIGNS: usize = 24;
        /// One-line chest labels — the common case, in quantity.
        const LABEL_SIGNS: usize = 120;

        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();

        let measure = |spawn: &SignSpawn| -> usize {
            let mut out = Vec::new();
            push_side_quads(
                &raster, &ink, &spawn.front, spawn.pos, spawn.kind, spawn.orientation, true,
                &mut out,
            );
            push_side_quads(
                &raster, &ink, &spawn.back, spawn.pos, spawn.kind, spawn.orientation, false,
                &mut out,
            );
            out.len()
        };

        let mut full = SignSpawn::at([0, 0, 0]);
        for i in 0..4 {
            let line = vec![plain_span("ABCDEFGHIJKLMNO")];
            full.front.lines[i] = line.clone();
            full.back.lines[i] = line;
        }
        let mut label = SignSpawn::at([0, 0, 0]);
        label.front.lines[1] = vec![plain_span("Redstone")];

        let full_cost = measure(&full);
        let label_cost = measure(&label);
        assert!(
            full_cost > 0 && label_cost > 0,
            "the fixture must produce ink to measure anything: full={full_cost},              label={label_cost}"
        );

        let full_demand = full_cost * FULL_SIGNS;
        let label_demand = label_cost * LABEL_SIGNS;
        println!(
            "one full sign {full_cost} vertices, one label {label_cost};              {FULL_SIGNS} full = {full_demand}, {LABEL_SIGNS} labels = {label_demand};              capacity {MAX_SIGN_TEXT_VERTICES}, previously {PREVIOUS_CAPACITY}"
        );

        // The wrong hypothesis, computed rather than described: at the old
        // capacity both populations overflow, which is what made whole signs
        // disappear. If this ever stops being true the fixture has drifted
        // and the arms below prove nothing.
        assert!(
            full_demand > PREVIOUS_CAPACITY && label_demand > PREVIOUS_CAPACITY,
            "the fixture no longer discriminates: at the previous capacity of              {PREVIOUS_CAPACITY} it must overflow, but measured {full_demand}              (full) and {label_demand} (labels)"
        );

        assert!(
            full_demand <= MAX_SIGN_TEXT_VERTICES,
            "{FULL_SIGNS} fully-written signs need {full_demand} vertices and the              pass can hold {MAX_SIGN_TEXT_VERTICES}. Every sign past the budget              draws no text at all, and which ones those are changes as the camera              moves — the reported flicker."
        );
        assert!(
            label_demand <= MAX_SIGN_TEXT_VERTICES,
            "{LABEL_SIGNS} one-line chest labels need {label_demand} vertices and              the pass can hold {MAX_SIGN_TEXT_VERTICES}"
        );
    }

    /// The budget is spent nearest-first, and signs strictly behind the eye
    /// are not spent on at all.
    ///
    /// Before this, `prepare` consumed `sign_spawns`' list in its own order —
    /// sorted by **block position** for pixel-gate determinism, which is
    /// unrelated to the camera. So an overflow dropped whichever signs sorted
    /// last, and moving the player changed the in-range set and therefore the
    /// cut, which is how a *whole* board blinks. The fixture puts the
    /// position-sort order deliberately at odds with the distance order, so a
    /// regression to "just use the caller's list" fails here.
    #[test]
    fn the_budget_is_spent_nearest_first_and_skips_signs_behind_the_eye() {
        let camera = Camera {
            position: glam::Vec3::new(0.0, 0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y_degrees: 70.0,
            aspect: 16.0 / 9.0,
            near: 0.05,
            far: 512.0,
        };
        // Yaw 0 faces `+Z` (`Camera::basis`), so `+Z` is in front and `-Z` is
        // behind. Listed here in **descending** distance so a function that
        // preserved its input order could not accidentally pass.
        let near = SignSpawn::at([0, 0, 4]);
        let mid = SignSpawn::at([0, 0, 20]);
        let far = SignSpawn::at([0, 0, 50]);
        let behind = SignSpawn::at([0, 0, -30]);
        let spawns = vec![far.clone(), behind.clone(), mid.clone(), near.clone()];

        let vp = camera.view_projection().to_cols_array_2d();
        let ordered = order_by_forward_distance(&vp, &spawns);
        let order: Vec<[i32; 3]> = ordered.iter().map(|s| s.pos).collect();
        assert_eq!(
            order,
            vec![near.pos, mid.pos, far.pos],
            "expected nearest-first with the sign behind the eye dropped"
        );

        // The control for the drop: a sign one block behind the eye plane is
        // within `BEHIND_EYE_SLACK` and must survive, so the filter is a
        // budget filter and not a visibility cull that could delete a board
        // the camera is standing inside.
        let straddling = SignSpawn::at([0, 0, -1]);
        let kept = order_by_forward_distance(&vp, std::slice::from_ref(&straddling));
        assert_eq!(
            kept.len(),
            1,
            "a sign at the eye plane must not be discarded — BEHIND_EYE_SLACK              exists so a board the camera is inside still draws"
        );
    }

    /// The central control: one line with no colour of its own must draw in
    /// the sign's own dye colour, and a *sibling* line carrying an explicit
    /// **hex** colour (not one of the sixteen legacy colours, so a lossy
    /// legacy-only path could not accidentally survive this) must draw in
    /// its own colour instead — never the dye's, and never the other way
    /// round. One fixture, both arms, so a fix that simply ignores the dye
    /// (every vertex the hex colour) or one that simply ignores per-run
    /// colour (every vertex the dye) both fail this.
    #[test]
    fn an_explicit_run_colour_wins_and_an_unset_run_falls_back_to_the_dye() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut side = SignSide::default();
        side.color = SignDyeColor::Red;
        side.lines[0] = vec![plain_span("A")];
        side.lines[1] = vec![SignTextSpan {
            text: "B".to_owned(),
            color: Some(0x0012_3456),
            ..Default::default()
        }];

        let mut out = Vec::new();
        push_side_quads(
            &raster,
            &ink,
            &side,
            [0, 0, 0],
            SignKind::Plain,
            SignOrientation::Ground { rotation_segment: 0 },
            true,
            &mut out,
        );
        assert!(!out.is_empty(), "the fixture must contribute ink to test anything");

        let unpack = |rgb: u32| {
            [
                ((rgb >> 16) & 0xFF) as f32 / 255.0,
                ((rgb >> 8) & 0xFF) as f32 / 255.0,
                (rgb & 0xFF) as f32 / 255.0,
            ]
        };
        let close = |a: [f32; 4], b: [f32; 3]| {
            (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3 && (a[2] - b[2]).abs() < 1e-3
        };
        let dye_rgb = unpack(default_run_color(&side));
        let hex_rgb = unpack(0x0012_3456);

        assert!(
            out.iter().any(|v| close(v.color, dye_rgb)),
            "no vertex drew in the sign's own dye colour {dye_rgb:?} — the \
             colour-less run's default fallback is broken"
        );
        assert!(
            out.iter().any(|v| close(v.color, hex_rgb)),
            "no vertex drew in the explicit hex colour {hex_rgb:?} — an \
             explicit run colour did not survive to the draw"
        );
        for v in &out {
            assert!(
                close(v.color, dye_rgb) || close(v.color, hex_rgb),
                "unexpected vertex colour {:?} — neither the dye {dye_rgb:?} \
                 nor the explicit hex colour {hex_rgb:?}",
                v.color
            );
        }
    }
}

