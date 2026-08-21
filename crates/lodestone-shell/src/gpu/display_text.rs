//! World-space `text_display` glyphs: coloured quads placed by
//! [`lodestone_render::display::text_glyph_transform`], reusing `gpu/nametag.rs`'s
//! jar-sourced font loader and ink-run layout — the same reuse
//! `gpu/sign_text.rs` already makes, and this file is structured identically
//! to that one; read its module doc for the shader/pipeline reasoning this
//! one does not repeat.
//!
//! # Not a billboard by default, but can become one
//!
//! Unlike [`super::nametag`] (always camera-facing) and unlike
//! [`super::sign_text`] (always a fixed world orientation), a `text_display`'s
//! orientation is **per-entity data** — `Display.BillboardConstraints`, one
//! of four modes — so the placement matrix is resolved per draw from
//! [`lodestone_render::display::display_orientation`] rather than baked into
//! this pass the way the other two bake in their one fixed choice.
//!
//! # Depth and blending
//!
//! No polygon-offset bias: unlike sign text (which shares a face with the
//! sign board's own terrain-mesh geometry and needs a bias to win the depth
//! tie), a `text_display`'s glyphs and background panel float in open space
//! with nothing coplanar to fight. Ordinary `LessEqual` depth compare,
//! straight alpha blending (`ALPHA_BLENDING`) for the translucent background
//! panel and partially-transparent glyphs alike.
//!
//! # What is deliberately not built (disclosed, not silent)
//!
//! - **Wrapping by [`DisplayDraw::text_line_width`] is not implemented.**
//!   Vanilla auto-wraps long lines to that pixel width; this pass only
//!   splits on the text's own literal `\n`. A long single "line" therefore
//!   overflows the background panel instead of wrapping. The field is still
//!   decoded and carried all the way to this draw site (see
//!   `crate::display_entities::DisplayDraw::text_line_width`'s own doc) — it
//!   is read, just not yet applied to a wrap algorithm.
//! - **`seeThrough`/`shadow` style-flag bits are decoded and not consumed**
//!   here — same simplification `gpu/nametag.rs`'s own module doc already
//!   makes for its background plate and per-glyph shadow.
//! - **Wrapping aside, per-run style now IS modelled** (colour, bold,
//!   italic, underline, strikethrough), via `gpu/nametag.rs::layout_styled_ink_runs`
//!   — the same styled ink-run walk `gpu/nametag.rs`'s own player/mob
//!   nametags now use. `textOpacity << 24 | 0xFFFFFF`
//!   (`TextDisplayRenderer.submitInner`, see
//!   [`lodestone_render::display::text_glyph_color`]'s doc) is real, but it
//!   is the **fallback** tint fed to `Font.java::getTextColor` for a span
//!   whose own colour is unspecified — not, as this file's doc previously
//!   and incorrectly claimed, a hardcode that discards a styled component's
//!   colour outright. A colour/bold/italic/underline/strikethrough-bearing
//!   `text_display` still needs the upstream flatten
//!   (`crates/protocol/v770`'s `Value::Text` decode, which currently reduces
//!   the NBT component to plain text via `plain_text_from_nbt_component`
//!   before this crate ever sees it) to stop discarding style before this
//!   pass can draw it — see this module's `push_text_display_quads` for the
//!   `Text::from_legacy` bridge that makes this pass style-ready the moment
//!   that upstream flatten changes.
//! - **A per-line width computed from *unstyled* advances mis-centres a
//!   line whose real (styled) content is wider — e.g. a bold run.** This was
//!   the file's own alignment defect: centring used
//!   `gpu/nametag.rs::layout_ink_runs`'s plain-codepoint width, which cannot
//!   see a bold run's extra advance (`GlyphInfo.getAdvance(bold)`,
//!   `Font.java`), so a two-line block whose second line carried a wider
//!   (bold, once style survives the upstream flatten above) run centred
//!   against too-small a width and read as shifted right. Switching to
//!   [`super::nametag::layout_styled_ink_runs`] for width too closes this,
//!   because that function's own advance accounts for bold.

use glam::{Mat4, Vec3};
use lodestone_assets::font::RasterFont;
use lodestone_model::text::Text;
use lodestone_render::display::{
    BillboardMode, DisplayTransformation, display_orientation, display_placement_matrix,
    text_background_color, text_glyph_color, text_glyph_transform,
};
use lodestone_render::sign::TEXT_LINE_HEIGHT;
use lodestone_render::{Camera, DEPTH_FORMAT};

use crate::display_entities::{DisplayDraw, TEXT_DISPLAY_TYPE_PATH};

/// Same vertex shape as `gpu/nametag.rs`'s `NameTagVertex`/`gpu/sign_text.rs`'s
/// `SignTextVertex` — kept as its own type per this crate's established
/// one-vertex-type-per-pass pattern.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayTextVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// Fixed vertex capacity, same fixed-buffer idiom as
/// [`super::nametag::MAX_NAME_TAG_VERTICES`]/[`super::sign_text::MAX_SIGN_TEXT_VERTICES`].
const MAX_DISPLAY_TEXT_VERTICES: usize = 40_000;

/// Draws world-space `text_display` glyphs and background panels — see the
/// module doc for why this is neither a pure billboard nor a fixed-orientation
/// pass, unlike its two nearest relatives.
#[derive(Debug)]
pub(super) struct DisplayTextRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: wgpu::Buffer,
    /// `None` off a jar-less run — same fail-open contract as
    /// [`super::nametag::NameTagRenderer::font`].
    font: Option<RasterFont>,
    /// Styled ink-run layouts, persisted across frames for the same reason
    /// `gpu/sign_text.rs::SignTextRenderer::ink` is. `Styled` (not
    /// `gpu/nametag.rs::InkLayoutCache`) so a coloured/bold/italic/underlined/
    /// struck-through span reaches this pass's geometry — see the module doc.
    ink: super::nametag::StyledInkLayoutCache,
}

impl DisplayTextRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-display-text-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/nametag.wgsl").into()),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-display-text-bgl"),
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
            label: Some("lodestone-display-text-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-display-text-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-display-text-vertices"),
            size: (MAX_DISPLAY_TEXT_VERTICES * std::mem::size_of::<DisplayTextVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-display-text-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<DisplayTextVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        })];

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lodestone-display-text-pipeline"),
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
                // No culling: the background panel and the glyph quads are
                // each built with an explicit, consistent winding, but a
                // `Center`-billboarded panel viewed from directly behind an
                // entity's own `Fixed` orientation has no "back face" concept
                // worth culling, matching `gpu/nametag.rs`/`gpu/sign_text.rs`.
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                // No bias — see the module doc's "Depth and blending" section
                // for why this pass has nothing coplanar to fight, unlike
                // `gpu/sign_text.rs`.
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
            font: super::nametag::load_font(),
            ink: super::nametag::StyledInkLayoutCache::default(),
        }
    }

    /// Uploads this frame's view-projection and `text_display` vertices.
    /// Must run before the render pass opens, same buffer-creation
    /// constraint as every other pass in this crate. Returns the vertex
    /// count, capped at [`MAX_DISPLAY_TEXT_VERTICES`] — pass to
    /// [`draw`](Self::draw).
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        draws: &[DisplayDraw],
        camera: &Camera,
    ) -> u32 {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));
        let Some(raster) = &self.font else {
            return 0;
        };

        let mut vertices = Vec::new();
        for draw in draws {
            if draw.type_path != TEXT_DISPLAY_TYPE_PATH {
                continue;
            }
            let Some(text) = &draw.text else { continue };
            if text.is_empty() {
                continue;
            }
            push_text_display_quads(raster, &self.ink, draw, text, camera, &mut vertices);
        }
        let len = vertices.len().min(MAX_DISPLAY_TEXT_VERTICES);
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

/// Lowers one `text_display`'s current text into world-space quads (the
/// background panel, when non-transparent, plus every non-empty line),
/// appended onto `out`.
fn push_text_display_quads(
    raster: &RasterFont,
    ink: &super::nametag::StyledInkLayoutCache,
    draw: &DisplayDraw,
    text: &str,
    camera: &Camera,
    out: &mut Vec<DisplayTextVertex>,
) {
    let lines: Vec<&str> = text.split('\n').collect();
    // `text` is still plain today (the protocol-layer flatten this pass
    // receives it from currently discards style before this crate ever sees
    // it — see the module doc), but `Text::from_legacy` is run unconditionally
    // rather than assuming plain text, so a `§`-coded line already draws
    // correctly and the moment the upstream flatten preserves style (as a
    // legacy-coded string, the same bridge `gpu/nametag.rs::push_entity_quads`
    // now uses) this pass needs no further change.
    //
    // Per-line layout up front: needed both to size the background panel
    // (vanilla's own `cachedInfo.width()`/`height()`, computed once before
    // any quad is emitted) and to lay out each line's glyphs afterwards —
    // computed once here rather than twice. Styled (not
    // `super::nametag::layout_ink_runs`'s plain width) so a bold run's real,
    // wider advance is what centring measures — see the module doc for the
    // alignment defect an unstyled width caused.
    let line_spans: Vec<_> = lines.iter().map(|line| Text::from_legacy(line).to_spans()).collect();
    let layouts: Vec<_> = line_spans.iter().map(|spans| ink.layout(raster, spans)).collect();
    let total_width = layouts.iter().map(|l| l.1).fold(0.0_f32, f32::max);
    let total_height = (lines.len() as f32).mul_add(TEXT_LINE_HEIGHT, -1.0);
    if total_width <= 0.0 {
        // Every line was empty (e.g. a lone `"\n"`) — vanilla's own
        // `Font.split` of an empty string contributes no ink either.
        return;
    }

    let orientation = display_orientation(
        draw.billboard,
        draw.entity_yaw,
        draw.entity_pitch,
        camera.yaw,
        camera.pitch,
    );
    let base = display_placement_matrix(draw.position, orientation, &draw.transform);
    let matrix = text_glyph_transform(base, total_width, total_height);

    // `Display.TextDisplay.getAlign`: neither bit set is centre, `0x08` is
    // left, `0x10` is right — ported directly rather than "simplified" to
    // always-centre, since a left/right-aligned sign board reads visibly
    // wrong against a centred one.
    let align_left = draw.text_style_flags & 0x08 != 0;
    let align_right = draw.text_style_flags & 0x10 != 0;

    if draw.text_background_color != 0 {
        push_background_quad(matrix, total_width, total_height, draw.text_background_color, out);
    }

    // `text_glyph_color`'s alpha is `textOpacity`'s own fraction — the real
    // per-frame value; its RGB (always white) is only the *fallback* a
    // `StyledRect` already carries for a colourless span, so only the alpha
    // channel is read here (see the module doc).
    let alpha = text_glyph_color(draw.text_opacity)[3];
    for (i, layout) in layouts.iter().enumerate() {
        let (rects, line_width) = (&layout.0, layout.1);
        if rects.is_empty() {
            continue;
        }
        let offset = if align_left {
            0.0
        } else if align_right {
            total_width - line_width
        } else {
            (total_width - line_width) / 2.0
        };
        let y_line = i as f32 * TEXT_LINE_HEIGHT;
        for rect in rects {
            let color = [rect.color[0], rect.color[1], rect.color[2], rect.color[3] * alpha];
            let lx = rect.x + offset;
            let ly = rect.y + y_line;
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
                DisplayTextVertex { position: tl, color },
                DisplayTextVertex { position: bl, color },
                DisplayTextVertex { position: tr, color },
                DisplayTextVertex { position: tr, color },
                DisplayTextVertex { position: bl, color },
                DisplayTextVertex { position: br, color },
            ]);
        }
    }
}

/// The background panel: local `(-1, -1)` to `(width, height)`, vanilla's
/// own four corners (`TextDisplayRenderer.submitInner`), split into two
/// triangles preserving the same winding vanilla's quad walk produces.
fn push_background_quad(
    matrix: Mat4,
    width: f32,
    height: f32,
    argb: i32,
    out: &mut Vec<DisplayTextVertex>,
) {
    let color = text_background_color(argb);
    let bl = matrix.transform_point3(Vec3::new(-1.0, -1.0, -0.01)).to_array();
    let tl = matrix.transform_point3(Vec3::new(-1.0, height, -0.01)).to_array();
    let tr = matrix.transform_point3(Vec3::new(width, height, -0.01)).to_array();
    let br = matrix.transform_point3(Vec3::new(width, -1.0, -0.01)).to_array();
    out.extend([
        DisplayTextVertex { position: bl, color },
        DisplayTextVertex { position: tl, color },
        DisplayTextVertex { position: tr, color },
        DisplayTextVertex { position: bl, color },
        DisplayTextVertex { position: tr, color },
        DisplayTextVertex { position: br, color },
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw_with_text(text: &str) -> DisplayDraw {
        DisplayDraw {
            id: 1,
            type_path: TEXT_DISPLAY_TYPE_PATH,
            position: Vec3::ZERO,
            entity_yaw: 0.0,
            entity_pitch: 0.0,
            billboard: BillboardMode::Fixed,
            transform: DisplayTransformation::default(),
            text: Some(text.to_owned()),
            text_line_width: 200,
            text_background_color: 0,
            text_opacity: -1,
            text_style_flags: 0,
            block_state: None,
            item: None,
            item_display_context: 0,
        }
    }

    /// An empty text (never reported, or explicitly cleared to "") must
    /// contribute nothing — mirrors `gpu/nametag.rs`'s/`gpu/sign_text.rs`'s
    /// own identical control for their own empty-input case.
    #[test]
    fn an_empty_text_contributes_no_vertices() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut out = Vec::new();
        push_text_display_quads(&raster, &ink, &draw_with_text(""), "", &Camera::default(), &mut out);
        assert!(out.is_empty());
    }

    /// The positive control paired with the empty-input one above: real text
    /// contributes real ink.
    #[test]
    fn real_text_contributes_vertices() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let draw = draw_with_text("LODESTONE");
        let mut out = Vec::new();
        push_text_display_quads(&raster, &ink, &draw, "LODESTONE", &Camera::default(), &mut out);
        assert!(!out.is_empty(), "real text must contribute vertices");
    }

    /// A non-zero background colour must add exactly six vertices (one quad,
    /// two triangles) beyond the glyphs alone — the discriminating check that
    /// the panel is actually gated on `argb != 0` and not drawn
    /// unconditionally (or never drawn at all).
    #[test]
    fn a_non_zero_background_color_adds_exactly_one_quad() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut without_bg = draw_with_text("A");
        without_bg.text_background_color = 0;
        let mut no_bg_out = Vec::new();
        push_text_display_quads(&raster, &ink, &without_bg, "A", &Camera::default(), &mut no_bg_out);

        let mut with_bg = draw_with_text("A");
        with_bg.text_background_color = 0x4000_0000_u32 as i32;
        let mut bg_out = Vec::new();
        push_text_display_quads(&raster, &ink, &with_bg, "A", &Camera::default(), &mut bg_out);

        assert_eq!(
            bg_out.len(),
            no_bg_out.len() + 6,
            "a non-zero background colour must add exactly one quad's worth \
             of vertices: no_bg={}, bg={}",
            no_bg_out.len(),
            bg_out.len()
        );
    }

    /// **The billboard-mode discriminating pair**, at the render call site
    /// rather than only inside `lodestone_render::display`'s own unit tests
    /// — two camera angles are the minimum needed to tell a billboard from a
    /// fixed quad (CLAUDE.md's evidence standard for this exact feature).
    /// `Fixed` must not move when only the camera rotates; a `Center`
    /// billboard on the same entity, camera pair must.
    #[test]
    fn fixed_text_does_not_track_the_camera_and_center_does() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut camera_a = Camera::default();
        camera_a.position = Vec3::new(5.0, 0.0, 0.0);
        camera_a.yaw = 10.0;
        camera_a.pitch = 5.0;
        let mut camera_b = Camera::default();
        camera_b.position = Vec3::new(5.0, 0.0, 0.0);
        camera_b.yaw = 200.0;
        camera_b.pitch = -35.0;

        let fixed_draw = draw_with_text("HELLO");
        let mut fixed_a = Vec::new();
        push_text_display_quads(&raster, &ink, &fixed_draw, "HELLO", &camera_a, &mut fixed_a);
        let mut fixed_b = Vec::new();
        push_text_display_quads(&raster, &ink, &fixed_draw, "HELLO", &camera_b, &mut fixed_b);
        assert_eq!(
            fixed_a, fixed_b,
            "Fixed billboard text must not move when only the camera rotates"
        );

        let mut center_draw = draw_with_text("HELLO");
        center_draw.billboard = BillboardMode::Center;
        let mut center_a = Vec::new();
        push_text_display_quads(&raster, &ink, &center_draw, "HELLO", &camera_a, &mut center_a);
        let mut center_b = Vec::new();
        push_text_display_quads(&raster, &ink, &center_draw, "HELLO", &camera_b, &mut center_b);
        assert_ne!(
            center_a, center_b,
            "Center billboard text must move when the camera rotates — \
             fixture cannot discriminate the two modes otherwise"
        );
    }

    /// **The colour control.** A `§c` (red) span must reach the draw with a
    /// real red vertex colour, and the same text with no colour code must
    /// not — the discriminating pair, not just "some colour appears",
    /// because a hardcoded non-white constant would pass a looser assertion.
    /// This is the control the module doc's alignment/colour fix claims to
    /// close: before `push_text_display_quads` read [`Text::from_legacy`]
    /// spans, every rect drew flat white (`text_glyph_color`) regardless of
    /// input, so this assertion would have failed against the pre-fix code.
    #[test]
    fn a_coloured_span_reaches_the_draw_with_its_colour_intact() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let red = lodestone_model::text::TextColor::Red.rgb();
        let want_red = [
            ((red >> 16) & 0xff) as f32 / 255.0,
            ((red >> 8) & 0xff) as f32 / 255.0,
            (red & 0xff) as f32 / 255.0,
        ];
        let is_red =
            |c: [f32; 4]| (c[0] - want_red[0]).abs() < 1e-3 && (c[1] - want_red[1]).abs() < 1e-3 && (c[2] - want_red[2]).abs() < 1e-3;

        let coloured = draw_with_text("\u{a7}cRED");
        let mut coloured_out = Vec::new();
        push_text_display_quads(&raster, &ink, &coloured, "\u{a7}cRED", &Camera::default(), &mut coloured_out);
        assert!(
            coloured_out.iter().any(|v| is_red(v.color)),
            "a §c-coloured span must reach the draw with red vertex colour, got: {:?}",
            coloured_out.iter().map(|v| v.color).collect::<Vec<_>>()
        );

        let plain = draw_with_text("RED");
        let mut plain_out = Vec::new();
        push_text_display_quads(&raster, &ink, &plain, "RED", &Camera::default(), &mut plain_out);
        assert!(
            !plain_out.iter().any(|v| is_red(v.color)),
            "plain (uncoloured) text must not draw red — the fixture must be \
             able to discriminate coloured from uncoloured, not merely see \
             \"some colour\""
        );
    }

    /// **The alignment control.** Bold widens a glyph's *advance*
    /// (`GlyphInfo.getAdvance(bold)`), so a bold run must measure wider than
    /// the identical codepoints unstyled — checked against
    /// [`super::super::nametag::layout_styled_ink_runs`], the exact
    /// expression `push_text_display_quads` itself calls through
    /// `StyledInkLayoutCache::layout`, not a restated constant. Two lines of
    /// deliberately different content (a short line and a long one) so the
    /// two hypotheses (bold-aware vs bold-blind width) cannot coincide.
    ///
    /// Before this file read styled spans, no input could make this
    /// assertion fail differently for bold vs plain — every line's width
    /// came from `gpu/nametag.rs::layout_ink_runs`'s plain per-codepoint
    /// advance, which cannot see `style.bold` at all — so this is the
    /// control that would have failed against the pre-fix code, and it is
    /// exactly the shape of defect the owner reported: a row whose real
    /// (styled) width is wider than what centring measured reads as shifted
    /// off-centre.
    #[test]
    fn a_bold_run_measures_wider_than_the_same_codepoints_unstyled() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let plain_spans = Text::from_legacy("WWWWWW").to_spans();
        let (_, plain_width) = super::super::nametag::layout_styled_ink_runs(&raster, &plain_spans);
        let bold_spans = Text::from_legacy("\u{a7}lWWWWWW").to_spans();
        let (_, bold_width) = super::super::nametag::layout_styled_ink_runs(&raster, &bold_spans);
        assert!(
            bold_width > plain_width,
            "a bold run must measure wider than the same codepoints unstyled: \
             plain={plain_width}, bold={bold_width}"
        );

        // And the effect reaches the actual centring offset a shorter,
        // unstyled sibling line would be drawn at: this is precisely
        // `push_text_display_quads`'s own `(total_width - line_width) / 2.0`
        // formula, evaluated here against the two hypotheses for the width
        // of the *other* line in the block.
        let short_spans = Text::from_legacy("Hi").to_spans();
        let (_, short_width) = super::super::nametag::layout_styled_ink_runs(&raster, &short_spans);
        let plain_block_offset = (plain_width - short_width) / 2.0;
        let bold_block_offset = (bold_width - short_width) / 2.0;
        assert!(
            (bold_block_offset - plain_block_offset).abs() > 1e-3,
            "widening one line with bold must move the other line's centring \
             offset (both derived from the same shared block width): \
             plain_offset={plain_block_offset}, bold_offset={bold_block_offset}"
        );

        // End-to-end: the same effect must be visible in the actual vertex
        // output of a two-line block whose second line is bold — the
        // block's total measured extent (max minus min world-space x, after
        // the full billboard/glyph transform) must grow.
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let plain_draw = draw_with_text("Hi\nWWWWWW");
        let mut plain_out = Vec::new();
        push_text_display_quads(&raster, &ink, &plain_draw, "Hi\nWWWWWW", &Camera::default(), &mut plain_out);
        let bold_draw = draw_with_text("Hi\n\u{a7}lWWWWWW");
        let mut bold_out = Vec::new();
        push_text_display_quads(&raster, &ink, &bold_draw, "Hi\n\u{a7}lWWWWWW", &Camera::default(), &mut bold_out);
        let extent = |verts: &[DisplayTextVertex]| -> f32 {
            let xs = verts.iter().map(|v| v.position[0]);
            let max = xs.clone().fold(f32::MIN, f32::max);
            let min = xs.fold(f32::MAX, f32::min);
            max - min
        };
        assert!(
            extent(&bold_out) > extent(&plain_out),
            "a bold second line must widen the drawn block's measured extent: \
             plain={:.4}, bold={:.4}",
            extent(&plain_out),
            extent(&bold_out)
        );
    }
}
