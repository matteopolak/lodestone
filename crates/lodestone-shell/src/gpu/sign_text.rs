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
//! # Colour space: this pass draws into a raw (non-sRGB) view
//!
//! Vanilla is not colour-managed, so a sign's resolved run colour —
//! `ARGB.scaleRGB(dye, 0.4)` for an unlit side, the full dye when glowing — is
//! a gamma byte written straight to the framebuffer. Every pipeline in this
//! crate targets the swapchain's *sRGB* view, which would encode it a second
//! time and read markedly lighter than vanilla. So this pass shares a render
//! pass with `gpu/display_text.rs` on the target's **raw** view, installed by
//! `RenderState::set_world_text_view`. Nothing about its position in the frame
//! changed: it still draws after the block entities and *before* the
//! translucent water, particles and weather, because a raindrop in front of a
//! sign must paint over it. See `docs/world-text-gamma-blend.md`.
//!
//! # Not a billboard
//!
//! Unlike [`super::nametag`], sign text has a fixed world orientation baked
//! into [`lodestone_render::sign_text_transform`] (the sign's own facing or
//! rotation), not a camera-facing basis — so there is no `right`/`up`
//! argument and no per-frame camera dependency beyond the shared `view_proj`
//! uniform every world-space pass in this crate already writes.
//!
//! # Glowing text's outline, as one dilated quad rather than eight copies
//!
//! `Font.prepare8xTextOutline` draws the whole string **eight** more times,
//! at every `(dx, dy)` in `{-1, 0, 1}²` except `(0, 0)`, each displaced by
//! `dx * glyph.getShadowOffset()`, in the outline colour — then
//! `TextFeatureRenderer` visits that output before the real glyphs.
//!
//! Eight copies of an ink-run layout would be an 8× vertex multiplier on a
//! pass that already has a fixed budget, and it is unnecessary here: the
//! union of a **rectangle** translated over that 3×3 neighbourhood is exactly
//! that rectangle grown by one offset on every side (dilation distributes
//! over union, and the `(0, 0)` copy is contained in the union of the two
//! horizontal ones). Every ink run [`super::nametag::layout_styled_ink_runs`]
//! emits *is* a rectangle, so one grown quad per run covers precisely the
//! region vanilla's eight copies cover, at 1× the vertices instead of 8×.
//! The outline colour is uniform and opaque, so the overlap the eight copies
//! have with each other is idempotent and nothing about the coverage differs.
//!
//! Two details that are not simplifications: the grow amount is the run's own
//! per-glyph [`StyledRect::outline_grow`](super::nametag::StyledRect)
//! (`GlyphInfo.getShadowOffset()`, half a pixel for a unihex glyph), and
//! underline/strikethrough bars carry `0.0` there and so contribute no
//! outline at all — vanilla's `outlineOutput.discardEffects()`.
//!
//! Vanilla submits the outline through `DisplayMode.NORMAL` and the glyphs
//! through `POLYGON_OFFSET`, i.e. the outline is *not* pulled toward the
//! camera and the glyphs are. This pass has one pipeline, so both get the
//! offset and the separation comes from submission order instead (outline
//! first, glyphs second, `LessEqual` depth). That is a real difference and it
//! is safe for the reason vanilla's own arrangement is: the two sets are
//! coplanar, and the glyphs must win.
//!
//! # Light: the branch that makes `has_glowing_text` mean anything
//!
//! `AbstractSignRenderer.submitSignText` sets three things off
//! `signText.hasGlowingText()`, and the third one is the light coordinate:
//! `15728880` (`LightCoordsUtil.FULL_BRIGHT` — sky 15 **and** block 15) when
//! glowing, `state.lightCoords` (the sign block's own sampled light) when
//! not. The glyph colour and the outline were ported first; this is the
//! third. Until it existed both arms produced identical pixels, because this
//! pass sampled no lightmap at all — so a glowing sign and a plain one were
//! equally bright in the dark, which is the one situation the feature exists
//! for.
//!
//! Three details, each of which is a way to get it wrong:
//!
//! * **The flag is per *side*.** A crimson sign can have plain front text and
//!   glowing back text; [`push_side_quads`] takes one [`SignSide`] and
//!   resolves the byte from *that* side's `glowing`, not from the spawn.
//! * **Glowing is [`super::nametag::TEXT_FULL_BRIGHT`] (`0xFF`), not
//!   [`lodestone_render::ENTITY_FULLBRIGHT`] (`0xF0`).** The latter is sky 15
//!   with block 0, and `sky_darken` scales the sky half — so at midnight it
//!   is not bright at all, which is exactly when a glowing sign has to be.
//! * **The outline takes the same byte as the glyphs.** Vanilla submits it
//!   through a different *display mode* but the same `lightVal`, and both
//!   `DisplayMode.NORMAL` and `POLYGON_OFFSET` select a render type whose
//!   shader does sample the lightmap (only `SEE_THROUGH` does not, and sign
//!   text is never see-through). The outline only exists for a glowing side
//!   anyway, so in practice it is always the full-bright byte.
//!
//! The sampled byte itself is [`SignSpawn::light`], which
//! `block_entities::sign_spawns` has been resolving through
//! `net::entity_light_at` all along — the producer was live and only the
//! consumer discarded it.

use glam::Vec3;
use lodestone_assets::font::RasterFont;
use lodestone_model::text::{TextColor, TextSpan, TextStyle};
use lodestone_render::{
    DEPTH_FORMAT, SignKind, SignOrientation, SignSpawn, sign_outline_color, sign_side_color,
    sign_text_transform,
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
    /// Kept for [`SignTextRenderer::set_color_format`] — see
    /// [`super::nametag::NameTagRenderer::bind_layout`] for why the layout
    /// object is retained rather than rebuilt from its descriptor.
    bind_layout: wgpu::BindGroupLayout,
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

        let pipeline = build_pipeline(device, &bind_layout, color_format);

        Self {
            pipeline,
            bind_layout,
            bind_group,
            uniform,
            vertices,
            font: super::nametag::load_font(),
            ink: super::nametag::StyledInkLayoutCache::default(),
        }
    }

    /// Rebuild the pipeline for a colour attachment of `color_format`, keeping
    /// the font, the ink cache and the vertex buffer. See
    /// [`super::nametag::NameTagRenderer::set_color_format`] for why the format
    /// cannot be settled when the renderer is built.
    pub(super) fn set_color_format(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) {
        self.pipeline = build_pipeline(device, &self.bind_layout, color_format);
    }
}

/// The one sign-text pipeline, for a colour attachment of `color_format` —
/// shared by [`SignTextRenderer::new`] and
/// [`SignTextRenderer::set_color_format`] so the ported depth state below is
/// written once.
fn build_pipeline(
    device: &wgpu::Device,
    bind_layout: &wgpu::BindGroupLayout,
    color_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("lodestone-sign-text-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/nametag.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("lodestone-sign-text-layout"),
        bind_group_layouts: &[Some(bind_layout)],
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

    pipeline
}

impl SignTextRenderer {
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
        eye: Vec3,
        signs: &[SignSpawn],
        light: super::nametag::WorldTextLight,
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
            // `isOutlineVisible` measures to the block's **centre**
            // (`Vec3.atCenterOf`), not to its corner or to the text plane.
            let centre = Vec3::new(
                spawn.pos[0] as f32 + 0.5,
                spawn.pos[1] as f32 + 0.5,
                spawn.pos[2] as f32 + 0.5,
            );
            let distance_squared = (centre - eye).length_squared();
            push_side_quads(
                raster,
                &self.ink,
                &spawn.front,
                spawn.pos,
                spawn.kind,
                spawn.orientation,
                true,
                distance_squared,
                light,
                spawn.light,
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
                distance_squared,
                light,
                spawn.light,
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

/// One line of a side, already truncated to the sign kind's own
/// `maxTextLineWidth` — `submitSignText`'s
/// `this.font.split(input, state.maxTextLineWidth)` followed by
/// `components.isEmpty() ? EMPTY : components.get(0)`.
///
/// **`split` word-wraps and vanilla then keeps only the first row**, so an
/// overlong line is *cut*, not shrunk and not scrolled. That is easy to
/// misread as a bug when you see it, and it is the reason a hanging sign's
/// text stays inside a board that is a third narrower than a standing one's:
/// `SignBlockEntity.getMaxTextLineWidth()` is 90 and
/// `HangingSignBlockEntity`'s override is 60, and nothing else in the two
/// renderers constrains the text horizontally at all.
///
/// Wraps through `crate::hud::wrap_spans_with`, this crate's one styled
/// word-wrap (the same greedy break-on-space / hard-break-an-overlong-word
/// body chat uses), measured by
/// [`super::nametag::styled_advance_width`] — vanilla measures its own split
/// with `StringSplitter`'s advance-only callback rather than by laying out
/// ink, and doing the same here keeps a wrap from costing more than the draw.
fn split_first_line(
    raster: &RasterFont,
    spans: &[TextSpan],
    max_width: f32,
) -> Vec<TextSpan> {
    let measure = |candidate: &[TextSpan]| super::nametag::styled_advance_width(raster, candidate);
    let mut rows = crate::hud::wrap_spans_with(measure, spans, max_width);
    if rows.is_empty() {
        // `wrap_spans_with` documents that it never returns an empty vector;
        // this is `components.isEmpty() ? FormattedCharSequence.EMPTY` anyway,
        // so the two agree on the degenerate case rather than one of them
        // panicking if the other's guarantee ever changes.
        return Vec::new();
    }
    rows.swap_remove(0)
}

/// Lowers one text side into world-space quads, appended onto `out`. A no-op
/// for a side whose four lines are all empty — vanilla's own font-split of
/// an empty string contributes no ink either, so this is an optimisation,
/// not a behaviour change.
///
/// `distance_squared` is from the camera to the sign block's centre, and is
/// used for exactly one thing: [`sign_outline_color`]'s 16-block gate.
#[allow(clippy::too_many_arguments)]
fn push_side_quads(
    raster: &RasterFont,
    ink: &super::nametag::StyledInkLayoutCache,
    side: &SignSide,
    pos: [i32; 3],
    kind: SignKind,
    orientation: SignOrientation,
    is_front: bool,
    distance_squared: f32,
    light: super::nametag::WorldTextLight,
    sampled_light: u8,
    out: &mut Vec<SignTextVertex>,
) {
    if side.lines.iter().all(Vec::is_empty) {
        return;
    }
    // `AbstractSignRenderer.submitSignText`'s third glow-gated quantity —
    // see the module doc. Per side, because `hasGlowingText` is.
    let tint = light.tint(if side.glowing {
        super::nametag::TEXT_FULL_BRIGHT
    } else {
        sampled_light
    });
    let matrix = sign_text_transform(pos, kind, orientation, is_front);
    let default_rgb = default_run_color(side);
    let outline = sign_outline_color(side, distance_squared);
    let max_width = kind.max_text_line_width();
    // `AbstractSignRenderer.submitSignText`: `signMidpoint = 4 *
    // textLineHeight / 2`, i.e. two full lines — line `i`'s top sits at
    // `i * textLineHeight - signMidpoint`. The height is the **block
    // entity's**, not a constant: a hanging sign overrides it to 9.
    let line_height = kind.text_line_height();
    let sign_midpoint = 2.0 * line_height;
    let mut quad = |rect_x: f32, rect_y: f32, w: f32, h: f32, color: [f32; 4]| {
        // The lightmap texel, folded in here rather than at either caller so
        // the outline and the glyphs cannot end up on different arms of the
        // glow branch — vanilla gives both the one `lightVal`.
        let color = super::nametag::tinted(color, tint);
        // Fed **unflipped** into the placement matrix: its own `-Y`
        // scale carries the pixel-space-down to world-space-up flip —
        // see `lodestone_render::sign`'s module doc.
        let tl = matrix
            .transform_point3(Vec3::new(rect_x, rect_y, 0.0))
            .to_array();
        let tr = matrix
            .transform_point3(Vec3::new(rect_x + w, rect_y, 0.0))
            .to_array();
        let bl = matrix
            .transform_point3(Vec3::new(rect_x, rect_y + h, 0.0))
            .to_array();
        let br = matrix
            .transform_point3(Vec3::new(rect_x + w, rect_y + h, 0.0))
            .to_array();
        out.extend([
            SignTextVertex { position: tl, color },
            SignTextVertex { position: bl, color },
            SignTextVertex { position: tr, color },
            SignTextVertex { position: tr, color },
            SignTextVertex { position: bl, color },
            SignTextVertex { position: br, color },
        ]);
    };
    for (i, line) in side.lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let spans = split_first_line(raster, &styled_spans(line, default_rgb), max_width);
        if spans.is_empty() {
            continue;
        }
        let layout = ink.layout(raster, &spans);
        let (rects, total_width) = (&layout.0, layout.1);
        // Each line is centred independently, matching `x1 =
        // -font.width(line) / 2` — not all four lines sharing one width, and
        // the width of the **truncated** line, since that is the sequence
        // vanilla hands to `font.width`. `total_width` already accounts for
        // bold's widened advance (see `layout_styled_ink_runs`'s doc), so a
        // bold line still centres correctly against its own (wider) width.
        let x1 = -total_width / 2.0;
        let y_off = i as f32 * line_height - sign_midpoint;
        // The outline first, the whole line's worth of it, then the glyphs —
        // `TextFeatureRenderer.buildGroup` visits `prepare8xTextOutline`'s
        // output before `prepareText`'s for exactly this reason, so a
        // neighbouring glyph's outline cannot paint over this glyph. Depth is
        // `LessEqual` and both sets are coplanar, so the later draw wins.
        if let Some(outline_color) = outline {
            for rect in rects.iter().filter(|r| r.outline_grow > 0.0) {
                let g = rect.outline_grow;
                quad(
                    rect.x + x1 - g,
                    rect.y + y_off - g,
                    rect.w + 2.0 * g,
                    rect.h + 2.0 * g,
                    outline_color,
                );
            }
        }
        for rect in rects {
            quad(rect.x + x1, rect.y + y_off, rect.w, rect.h, rect.color);
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
            // Well inside `OUTLINE_RENDER_DISTANCE_SQUARED`, so a glowing
            // side in these fixtures is outlined; a non-glowing one never is.
            0.0,
            super::super::nametag::WorldTextLight::overworld_noon(),
            super::super::nametag::TEXT_FULL_BRIGHT,
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
            // Well inside `OUTLINE_RENDER_DISTANCE_SQUARED`, so a glowing
            // side in these fixtures is outlined; a non-glowing one never is.
            0.0,
            super::super::nametag::WorldTextLight::overworld_noon(),
            super::super::nametag::TEXT_FULL_BRIGHT,
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
            // Well inside `OUTLINE_RENDER_DISTANCE_SQUARED`, so a glowing
            // side in these fixtures is outlined; a non-glowing one never is.
            0.0,
            super::super::nametag::WorldTextLight::overworld_noon(),
            super::super::nametag::TEXT_FULL_BRIGHT,
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
            // Well inside `OUTLINE_RENDER_DISTANCE_SQUARED`, so a glowing
            // side in these fixtures is outlined; a non-glowing one never is.
            0.0,
            super::super::nametag::WorldTextLight::overworld_noon(),
            super::super::nametag::TEXT_FULL_BRIGHT,
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
                &raster, &ink, &spawn.front, spawn.pos, spawn.kind, spawn.orientation, true, 0.0,
                super::super::nametag::WorldTextLight::overworld_noon(), super::super::nametag::TEXT_FULL_BRIGHT,
                &mut out,
            );
            push_side_quads(
                &raster, &ink, &spawn.back, spawn.pos, spawn.kind, spawn.orientation, false, 0.0,
                super::super::nametag::WorldTextLight::overworld_noon(), super::super::nametag::TEXT_FULL_BRIGHT,
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

    /// **The hanging-sign width gate**, at [`split_first_line`]. A line wider
    /// than a hanging sign's own `maxTextLineWidth` (60) but inside a
    /// standing sign's (90) must be **cut** on the hanging board and kept
    /// whole on the standing one.
    ///
    /// This gates the split alone: it passes the cap in by hand, so it cannot
    /// see whether [`push_side_quads`] fetches the *right* cap for the kind
    /// it is drawing — measured, by neutering that call site to
    /// `SignKind::Plain`, which leaves this green.
    /// `hanging_sign_text_stays_inside_its_board` is the arm that catches
    /// that, and the pair is deliberate rather than redundant.
    ///
    /// Both hypotheses are evaluated on one input, which is the only way this
    /// can fail for the right reason: the defect it exists for was
    /// `push_side_quads` applying no cap at all, and a cap that (wrongly)
    /// used `SignKind::Plain`'s 90 for every kind is the other plausible
    /// mistake — it keeps the whole line on *both* boards and fails the
    /// hanging arm here. The fixture width is asserted to sit strictly
    /// between the two caps first, so an input that could not tell them apart
    /// fails as a bad fixture rather than passing vacuously.
    #[test]
    fn a_hanging_sign_cuts_a_line_a_standing_sign_keeps_whole() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let spans = vec![TextSpan {
            text: "narrower board".to_owned(),
            style: TextStyle::default(),
        }];
        let full = super::super::nametag::styled_advance_width(&raster, &spans);
        let hanging_cap = SignKind::Hanging.max_text_line_width();
        let plain_cap = SignKind::Plain.max_text_line_width();
        assert!(
            full > hanging_cap && full <= plain_cap,
            "the fixture must be discriminating: {full} px against caps \
             {hanging_cap}/{plain_cap}"
        );

        let kept_plain = split_first_line(&raster, &spans, plain_cap);
        assert_eq!(
            kept_plain, spans,
            "a standing sign must keep this line exactly as written"
        );

        let kept_hanging = split_first_line(&raster, &spans, hanging_cap);
        let kept_text: String = kept_hanging.iter().map(|s| s.text.as_str()).collect();
        assert_ne!(
            kept_text, "narrower board",
            "a hanging sign must cut this line, not draw it whole"
        );
        assert!(
            !kept_text.is_empty(),
            "the cut must keep the first wrapped row, not drop the line"
        );
        let kept_width = super::super::nametag::styled_advance_width(&raster, &kept_hanging);
        assert!(
            kept_width <= hanging_cap,
            "the kept row {kept_text:?} measures {kept_width} px, over the \
             hanging cap of {hanging_cap}"
        );
    }

    /// The same claim one layer down, in **world space**, where the reported
    /// defect actually lives: the drawn quads must stay inside the board.
    ///
    /// The bound is derived rather than eyeballed —
    /// `maxTextLineWidth / 2 * renderScale` is the half-width of the widest
    /// line vanilla can produce, and a hanging sign's is
    /// `60 / 2 * 0.0140625 = 0.421875` blocks against
    /// `block/template_hanging_sign`'s own 14/16-wide board (half-width
    /// `0.4375`). So a correct line fits the board with 1/64 of a block to
    /// spare, and the uncapped line does not. The control is the *same*
    /// fixture measured without the cap, required to exceed the board.
    #[test]
    fn hanging_sign_text_stays_inside_its_board() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut side = SignSide::default();
        side.lines[0] = vec![plain_span("narrower board")];

        // `rotation_segment: 0` faces +Z, so the text runs along world X and
        // the board's own half-width is the X extent from the block centre.
        let mut out = Vec::new();
        push_side_quads(
            &raster,
            &ink,
            &side,
            [0, 0, 0],
            SignKind::Hanging,
            SignOrientation::Ground { rotation_segment: 0 },
            true,
            0.0,
            super::super::nametag::WorldTextLight::overworld_noon(),
            super::super::nametag::TEXT_FULL_BRIGHT,
            &mut out,
        );
        assert!(!out.is_empty(), "the fixture must draw something at all");
        let extent = out
            .iter()
            .map(|v| (v.position[0] - 0.5).abs())
            .fold(0.0f32, f32::max);

        const BOARD_HALF_WIDTH: f32 = 7.0 / 16.0;
        let cap_half_width =
            SignKind::Hanging.max_text_line_width() / 2.0 * SignKind::Hanging.render_scale();
        assert!(
            (cap_half_width - 0.421_875).abs() < 1e-6,
            "the derived bound moved: {cap_half_width}"
        );
        assert!(
            extent <= cap_half_width,
            "hanging sign text reaches {extent} blocks from the board centre, \
             past its own {cap_half_width}-block text area (the board itself \
             is {BOARD_HALF_WIDTH})"
        );

        // The control, so a pass here cannot mean "the fixture was short
        // enough anyway": the *uncapped* width of the same line overhangs the
        // board on both sides.
        let uncapped_half = super::super::nametag::styled_advance_width(
            &raster,
            &[TextSpan {
                text: "narrower board".to_owned(),
                style: TextStyle::default(),
            }],
        ) / 2.0
            * SignKind::Hanging.render_scale();
        assert!(
            uncapped_half > BOARD_HALF_WIDTH,
            "this fixture cannot see the defect: uncapped it reaches only \
             {uncapped_half} blocks, inside the {BOARD_HALF_WIDTH}-block board"
        );
    }

    /// **The glowing-outline gate.** A glowing side draws vanilla's outline
    /// behind its glyphs; a non-glowing one draws none.
    ///
    /// The count is a **prediction**, not a direction: the outline is one
    /// quad per glyph ink run, so the glowing vertex total must be exactly
    /// `plain + 6 * <glyph runs>`, with the run count read off the layout
    /// rather than off the thing under test. The wrong hypotheses it
    /// separates are "no outline at all" (equal totals) and "outline the
    /// effect bars too" (`discardEffects`), which the underlined fixture
    /// line exists for.
    #[test]
    fn a_glowing_side_draws_an_outline_behind_its_glyphs_and_a_plain_one_does_not() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut side = SignSide::default();
        side.color = SignDyeColor::Lime;
        side.lines[0] = vec![plain_span("glowing")];
        // A second line whose runs carry an underline, so `discardEffects` is
        // exercised: its effect bars must contribute no outline quads.
        side.lines[1] = vec![SignTextSpan {
            text: "underlined".to_owned(),
            underlined: true,
            ..Default::default()
        }];

        let draw = |side: &SignSide, distance_squared: f32| {
            let mut out = Vec::new();
            push_side_quads(
                &raster,
                &ink,
                side,
                [0, 0, 0],
                SignKind::Plain,
                SignOrientation::Ground { rotation_segment: 0 },
                true,
                distance_squared,
                super::super::nametag::WorldTextLight::overworld_noon(),
                super::super::nametag::TEXT_FULL_BRIGHT,
                &mut out,
            );
            out
        };

        let plain = draw(&side, 0.0);
        assert!(!plain.is_empty(), "the fixture must draw ink at all");

        let mut glowing_side = side.clone();
        glowing_side.glowing = true;
        let glowing = draw(&glowing_side, 0.0);

        // The expected extra, computed from the layouts themselves.
        let glyph_runs: usize = [0usize, 1]
            .into_iter()
            .map(|i| {
                let spans = styled_spans(&glowing_side.lines[i], default_run_color(&glowing_side));
                let layout = ink.layout(&raster, &spans);
                layout.0.iter().filter(|r| r.outline_grow > 0.0).count()
            })
            .sum();
        assert!(glyph_runs > 0, "the fixture produced no glyph ink runs");
        assert_eq!(
            glowing.len(),
            plain.len() + 6 * glyph_runs,
            "glowing must add exactly one quad per glyph ink run and none for \
             the underline bars ({glyph_runs} runs)"
        );

        // And the added quads are the dark colour, which is *not* the glyph
        // colour on a glowing side — the whole point of the outline.
        let outline = lodestone_render::sign_outline_color(&glowing_side, 0.0)
            .expect("a glowing lime sign within 16 blocks is outlined");
        let glyph = sign_side_color(&glowing_side);
        assert_ne!(outline, glyph, "the outline must differ from the glyphs");
        let outline_quads = glowing.iter().filter(|v| v.color == outline).count();
        assert_eq!(outline_quads, 6 * glyph_runs);

        // A non-glowing side draws *only* its own rects, no outline pass at
        // all. This cannot be asserted by colour: a plain side's glyph colour
        // **is** `getDarkColor`, the same value the outline uses, so the two
        // coincide by construction — a colour filter would count every glyph
        // as an outline. The vertex total against the layout's own rect count
        // is the assertion that separates them.
        let all_runs: usize = [0usize, 1]
            .into_iter()
            .map(|i| {
                let spans = styled_spans(&side.lines[i], default_run_color(&side));
                ink.layout(&raster, &spans).0.len()
            })
            .sum();
        assert_eq!(
            plain.len(),
            6 * all_runs,
            "a non-glowing side must draw one quad per layout rect and nothing more"
        );
    }

    /// The outline's distance gate, both arms, on the same fixture — and the
    /// black arm, which ignores distance entirely.
    ///
    /// `Mth.square(16) == 256`, so 15 blocks (225) is inside and 17 (289) is
    /// out; neither input sits on the boundary, where `<` and `<=` cannot be
    /// told apart.
    #[test]
    fn the_outline_fades_out_past_sixteen_blocks_except_for_black_text() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut side = SignSide::default();
        side.glowing = true;
        side.color = SignDyeColor::Lime;
        side.lines[0] = vec![plain_span("glowing")];

        let count = |side: &SignSide, distance_squared: f32| {
            let mut out = Vec::new();
            push_side_quads(
                &raster,
                &ink,
                side,
                [0, 0, 0],
                SignKind::Plain,
                SignOrientation::Ground { rotation_segment: 0 },
                true,
                distance_squared,
                super::super::nametag::WorldTextLight::overworld_noon(),
                super::super::nametag::TEXT_FULL_BRIGHT,
                &mut out,
            );
            out.len()
        };

        let near = count(&side, 15.0 * 15.0);
        let far = count(&side, 17.0 * 17.0);
        assert!(
            near > far,
            "a glowing lime sign must lose its outline past 16 blocks: \
             {near} vertices near, {far} far"
        );

        let mut black = side.clone();
        black.color = SignDyeColor::Black;
        assert_eq!(
            count(&black, 17.0 * 17.0),
            near,
            "black glowing text is outlined at any range — otherwise its \
             colour-0 glyphs are invisible"
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
            // Well inside `OUTLINE_RENDER_DISTANCE_SQUARED`, so a glowing
            // side in these fixtures is outlined; a non-glowing one never is.
            0.0,
            super::super::nametag::WorldTextLight::overworld_noon(),
            super::super::nametag::TEXT_FULL_BRIGHT,
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

    /// Lays one side out at `sampled` packed light and returns its vertices.
    /// Everything else is fixed, so the only thing separating two calls is
    /// the light.
    fn side_at_light(
        raster: &RasterFont,
        side: &SignSide,
        sampled: u8,
        light: super::super::nametag::WorldTextLight,
    ) -> Vec<SignTextVertex> {
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let mut out = Vec::new();
        push_side_quads(
            raster,
            &ink,
            side,
            [0, 0, 0],
            SignKind::Plain,
            SignOrientation::Ground { rotation_segment: 0 },
            true,
            // Inside the outline gate, so a glowing side here is outlined —
            // which is deliberate: the outline must take the same light byte
            // as the glyphs, and a fixture that suppressed it could not say so.
            0.0,
            light,
            sampled,
            &mut out,
        );
        out
    }

    /// A white-dyed side carrying one glyph. White is chosen so the two
    /// colours this pass can emit are the extremes it has — a glowing side's
    /// glyphs are the **full** dye (`1.0`) and its outline is
    /// `scaleRGB(dye, 0.4)` — which keeps a mis-scaled vertex obvious. It is
    /// not assumed to be `1.0` anywhere: every assertion below measures
    /// against the *same fixture drawn at full-bright light*, so the base
    /// colour cancels and what is left is the lightmap texel alone.
    fn white_side(glowing: bool) -> SignSide {
        let mut side = SignSide {
            glowing,
            color: SignDyeColor::White,
            ..Default::default()
        };
        side.lines[0] = vec![plain_span("A")];
        side
    }

    /// Every vertex of `measured` equals the matching vertex of `reference`
    /// scaled by `tint`, or the list of the ones that do not.
    ///
    /// Collected rather than asserted in the loop: an `assert!` inside the
    /// walk proves exactly one vertex and turns the rest into an argument
    /// instead of an observation.
    fn tint_mismatches(
        measured: &[SignTextVertex],
        reference: &[SignTextVertex],
        tint: [f32; 3],
    ) -> Vec<(usize, [f32; 4], [f32; 3])> {
        measured
            .iter()
            .zip(reference)
            .enumerate()
            .filter_map(|(i, (m, r))| {
                let want = [r.color[0] * tint[0], r.color[1] * tint[1], r.color[2] * tint[2]];
                let ok = (0..3).all(|c| (m.color[c] - want[c]).abs() < 1e-4);
                (!ok).then_some((i, m.color, want))
            })
            .collect()
    }

    /// The whole point of the light port, stated as the thing that was false
    /// before it: **two sides differing only in `glowing`, in the same dark
    /// cell, must not render the same.**
    ///
    /// Both arms are predicted, not merely ordered, and each is measured
    /// against the *same side drawn at full-bright light* so the dye and the
    /// outline scale cancel out. The glowing arm must be byte-identical to
    /// that reference — which is also the regression control for "this fix
    /// must not make glowing text dimmer than it already was". The plain arm
    /// must be the reference scaled by `light_color(0x00, …)`, the
    /// ambient-only floor. Without the second half the gate would pass on the
    /// pre-fix behaviour, where both arms were full bright.
    #[test]
    fn a_glowing_side_stays_full_bright_in_the_dark_and_a_plain_one_does_not() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let light = super::super::nametag::WorldTextLight::overworld_noon();
        // Pitch dark: no sky reaches the cell and nothing emits.
        const DARK: u8 = 0x00;
        const BRIGHT: u8 = super::super::nametag::TEXT_FULL_BRIGHT;
        let ambient = lodestone_render::light::OVERWORLD_AMBIENT_LIGHT;

        let want_dark = lodestone_render::light_color(DARK, 1.0, ambient);
        assert_eq!(
            lodestone_render::light_color(BRIGHT, 1.0, ambient),
            [1.0, 1.0, 1.0],
            "vanilla's FULL_BRIGHT must resolve to an identity tint, or the \
             glow arm is being dimmed by the very fix it is immune to"
        );
        assert!(
            want_dark[0] < 0.5,
            "the fixture's dark cell must actually be dark under the lightmap \
             curve, or neither arm can discriminate: got {want_dark:?}"
        );

        let glow_dark = side_at_light(&raster, &white_side(true), DARK, light);
        let glow_bright = side_at_light(&raster, &white_side(true), BRIGHT, light);
        let plain_dark = side_at_light(&raster, &white_side(false), DARK, light);
        let plain_bright = side_at_light(&raster, &white_side(false), BRIGHT, light);
        assert!(
            !glow_dark.is_empty() && !plain_dark.is_empty(),
            "the fixture must draw ink on both arms to compare anything"
        );

        let glow_bad = tint_mismatches(&glow_dark, &glow_bright, [1.0, 1.0, 1.0]);
        assert!(
            glow_bad.is_empty(),
            "a glowing side must ignore its block's sampled light entirely — \
             that is the situation the feature exists for. {} of {} vertices \
             moved between a dark cell and a bright one, e.g. {:?}",
            glow_bad.len(),
            glow_dark.len(),
            glow_bad.first()
        );

        let plain_bad = tint_mismatches(&plain_dark, &plain_bright, want_dark);
        assert!(
            plain_bad.is_empty(),
            "a plain side in a dark cell must be its full-bright self scaled \
             by {want_dark:?}, the lightmap texel for its own block. {} of {} \
             vertices did not match, e.g. (index, got, want) {:?}",
            plain_bad.len(),
            plain_dark.len(),
            plain_bad.first()
        );
        assert!(
            plain_dark
                .iter()
                .zip(&plain_bright)
                .any(|(a, b)| (a.color[0] - b.color[0]).abs() > 0.05),
            "the plain arm must actually have moved — identical vertices in a \
             dark and a bright cell is exactly the pre-fix behaviour"
        );
    }

    /// Same side, same everything, three different **sampled** light bytes:
    /// the vertices must get monotonically brighter. This is the
    /// producer-facing half — a fixture where every sign sits in the same
    /// light cannot see a pass that ignores the byte, and that is exactly how
    /// this gap survived.
    ///
    /// The bytes are chosen so no two coincide. Block light 14 does **not**
    /// work: `BLOCK_FACTOR` is `1.4`, so `brightness(14/15) * 1.4` already
    /// clamps to `1.0` and a torch reads identical to open sky at noon —
    /// measured, after the first version of this gate compared `0.4` with
    /// `0.4` and reported a defect that was not there.
    #[test]
    fn two_identical_plain_sides_in_different_light_render_differently() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let light = super::super::nametag::WorldTextLight::overworld_noon();
        let side = white_side(false);

        let dark = side_at_light(&raster, &side, 0x00, light);
        let dim = side_at_light(&raster, &side, 0x04, light);
        let daylit = side_at_light(&raster, &side, 0xF0, light);

        assert!(!dark.is_empty(), "the fixture must draw ink");
        assert_eq!(
            dark.len(),
            dim.len(),
            "the arms must differ only in colour, never in geometry"
        );

        let brightness = |v: &[SignTextVertex]| v[0].color[0];
        assert!(
            brightness(&dark) < brightness(&dim),
            "a sign in a pitch-dark cell must be dimmer than the same sign at \
             block light 4: {} vs {}",
            brightness(&dark),
            brightness(&dim)
        );
        assert!(
            brightness(&dim) < brightness(&daylit),
            "block light 4 must be dimmer than open sky at noon: {} vs {}",
            brightness(&dim),
            brightness(&daylit)
        );
        // Geometry is identical, so no positional field may have moved.
        for (a, b) in dark.iter().zip(&daylit) {
            assert_eq!(a.position, b.position, "light must not move a vertex");
        }
    }

    /// The glow arm's byte is `0xFF` — sky 15 **and** block 15 — not
    /// `ENTITY_FULLBRIGHT`'s `0xF0`, whose sky half the clock scales.
    ///
    /// The two candidates coincide at noon, which is why this gate runs at
    /// midnight: `sky_darken` is `0.24` there, so the wrong constant leaves a
    /// glowing sign markedly dim exactly when a player needs to read it. The
    /// measurement is against the same side at noon, so the dye and the
    /// outline scale cancel and what is compared is the two candidate tints.
    #[test]
    fn a_glowing_side_survives_midnight_which_the_sky_only_full_bright_byte_would_not() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        // This project's port of `SKY_LIGHT_FACTOR` at midnight.
        const MIDNIGHT: f32 = 0.24;
        let ambient = lodestone_render::light::OVERWORLD_AMBIENT_LIGHT;

        let right =
            lodestone_render::light_color(super::super::nametag::TEXT_FULL_BRIGHT, MIDNIGHT, ambient);
        let wrong = lodestone_render::light_color(lodestone_render::ENTITY_FULLBRIGHT, MIDNIGHT, ambient);
        assert!(
            right[0] - wrong[0] > 0.3,
            "the two candidate full-bright bytes must actually disagree at \
             midnight, or this gate measures nothing: {right:?} vs {wrong:?}"
        );

        let noon = super::super::nametag::WorldTextLight::overworld_noon();
        let midnight = super::super::nametag::WorldTextLight::new(MIDNIGHT, ambient);
        let reference = side_at_light(&raster, &white_side(true), 0x00, noon);
        let measured = side_at_light(&raster, &white_side(true), 0x00, midnight);
        assert!(!measured.is_empty(), "the fixture must draw ink");

        let bad = tint_mismatches(&measured, &reference, right);
        assert!(
            bad.is_empty(),
            "glowing sign text at midnight must land on {right:?} (sky 15 AND \
             block 15) rather than {wrong:?} (sky only): {} of {} vertices \
             wrong, e.g. (index, got, want) {:?}",
            bad.len(),
            measured.len(),
            bad.first()
        );
    }
}
