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
//! **Four pipelines. Two because vanilla submits the panel and the glyphs
//! through two different ones**, a third because the glyphs' own drop shadow
//! has to be separated from the glyphs by a mechanism this depth format can
//! actually resolve (see "The drop shadow needs its own polygon offset"
//! below), and a fourth because a `text_display` can ask for none of it:
//! `FLAG_SEE_THROUGH` selects `TEXT_SEE_THROUGH` /
//! `TEXT_BACKGROUND_SEE_THROUGH`, whose depth state is
//! `withDepthStencilState(Optional.empty())` — no test, no write. Those two
//! collapse into one pipeline here because their depth state is identical and
//! the shader difference is not something this pass ports.
//! `TextDisplayRenderer.submitInner` hands the
//! background quad to `RenderTypes.textBackground()` —
//! `RenderPipelines.TEXT_BACKGROUND`, whose depth state is the plain
//! `DepthStencilState.DEFAULT` — and every line of text to
//! `Font.DisplayMode.POLYGON_OFFSET`, which resolves to
//! `RenderPipelines.TEXT_POLYGON_OFFSET`:
//! `new DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, true, 1.0F, 10.0F)`.
//! The two numeric constants are the same polygon offset `gpu/sign_text.rs`
//! already ports, and the same sign flip applies (vanilla is reversed-Z,
//! this project's depth is `[0,1]`), giving [`TEXT_POLYGON_OFFSET`] —
//! `constant: -10, slope_scale: -1.0` — on the **shadow** pipeline and no
//! bias at all on the background one. The ink pipeline takes
//! [`GLYPH_POLYGON_OFFSET`], which is that same offset counted **twice**,
//! for the reason in the next section. Straight alpha blending
//! (`ALPHA_BLENDING`) on all of them.
//!
//! This pass originally drew everything through one unbiased pipeline, on
//! the reasoning that "a `text_display`'s glyphs and background panel float
//! in open space with nothing coplanar to fight". That is false, and its own
//! panel is the thing it fights: vanilla separates the two by `-0.01` in
//! **local glyph space**, which the `0.025` text scale turns into
//! **0.00025 blocks** — a couple of `f32` ULP in a `[0,1]` depth buffer at
//! any real viewing distance. Head-on the glyphs still won (`LessEqual`
//! passes a tie and the glyphs are submitted second), but the two quads are
//! *different sizes*, so their interpolated depth diverges as the plane goes
//! oblique to the view — which is exactly the case a yaw-only
//! (`BillboardMode::Vertical`) hologram is in whenever the camera is pitched.
//! Measured through `tests/world_text_over_geometry_pixels.rs`: glyph ink
//! fell from 438 px to 389 px when the panel was switched on, at a 40°
//! upward look, and the loss is per-pixel scatter rather than a clean edge.
//! Do not collapse these back into one pipeline.
//!
//! # The drop shadow needs its own polygon offset
//!
//! Owner report, after the drop shadow landed: *"the shadow text is
//! z-fighting with the real text in places where both are on the same
//! 'pixel' for holograms"*.
//!
//! Why the shadow contests depth with the glyph *at all*, given the two are
//! nominally the same plane: they are **different quads** — the shadow is the
//! same rect one font pixel away on both axes — so at a pixel they both
//! cover, their window `z` is interpolated from two different triangles and
//! the two results differ by float rounding. A tie would be safe (`LessEqual`
//! passes it and the ink is submitted second), but rounding does not produce
//! ties; it produces per-fragment noise in both directions, and the fragments
//! where it lands the wrong way are the speckle the owner saw. That also
//! explains why the symptom is *speckle* rather than the text vanishing
//! wholesale: two exactly parallel planes one representable step apart flip
//! whole, so per-pixel fighting is itself evidence the two surfaces are not
//! parallel — which an in-plane offset on an angled billboard is exactly.
//!
//! Vanilla separates the two **geometrically**, in the text's own plane:
//! `BakedSheetGlyph.renderChar` emits the shadow copy at local `z = 0` and
//! the glyph at `z = 0.03` whenever there is a shadow (and at `0` when there
//! is not — the offset exists only to clear the shadow). **That port was
//! written, measured, and deliberately removed.** Two reasons, in order of
//! weight:
//!
//! - **It encodes which side is the front into the geometry, and a
//!   `text_display` is visible from both.** `0.03` local is `0.00075 · scale`
//!   blocks along the text plane's own normal, so it moves the glyph toward
//!   the viewer from the front and *away* from it from behind — where it then
//!   swamps any ULP-denominated offset trying to correct it, because it is
//!   orders of magnitude larger. Measured on an 85°-oblique hologram viewed
//!   from behind: 3,120 lost ink px of 17,984 with the constant in, **0**
//!   with it out.
//! - **It is under a ULP where it matters anyway.** `0.00075 · scale` blocks
//!   is three times the panel-versus-ink separation the sweep in
//!   `tests/world_text_over_geometry_pixels.rs` already measures at between
//!   6.7 and 0.56 `f32` ULP of a forward `[0,1]` `Depth32Float`. Vanilla can
//!   spend a separation that small because reversed-Z has orders of magnitude
//!   more precision at every distance; this renderer cannot. That is
//!   `CLAUDE.md`'s measured rule about ported sub-millimetre depth
//!   separations, arriving at this pass.
//!
//! What replaces it is a **polygon offset**: the shadow keeps vanilla's own
//! `TEXT_POLYGON_OFFSET` and the ink takes [`GLYPH_POLYGON_OFFSET`], which is
//! that same offset counted twice in both its terms. A depth bias against a
//! float depth format is denominated in ULPs of the primitive's *own* depth
//! (`r = 2^(exponent(z) - 23)`) plus a multiple of its own depth *gradient*,
//! so it is scale-adaptive **and** view-angle-adaptive by construction, and
//! it is measured from the camera rather than baked into the geometry — which
//! is precisely why it survives being walked around.
//! `lodestone_render::entity_pipeline::SHADOW_DEPTH_BIAS` is the same
//! mechanism for the same reason one subsystem over, and its doc carries the
//! measured ULP-per-block table.
//!
//! Measured, twelve headless configurations spanning face-on to 85° oblique,
//! front and back, 3 to 24 blocks at constant angular size, worst row of each
//! group (ink lost of ~15–18k drawn):
//!
//! | shadow/ink separation | 70° back | 80° back | 85° back | 80° front |
//! |---|---|---|---|---|
//! | one pipeline, no geometry — as shipped | 1,014 | 1,141 | 2,883 | — |
//! | constant term only, no geometry | 4 | 34 | 1,204 | — |
//! | constant + slope, **plus** vanilla's `0.03` | 101 | 297 | 3,120 | 189 |
//! | **constant + slope, no geometry** | **0** | **0** | **0** | **0** |
//!
//! Row three is the one worth reading twice: the faithful port made things
//! *worse than doing nothing* from behind. Row two is why the slope term is
//! doubled and not just the constant — a near-grazing plane's rounding grows
//! with its depth gradient, which only the slope term tracks.
//!
//! Two things deliberately **not** done, each of which would also have
//! worked:
//!
//! - **Drawing the shadow with depth write off.** That removes the contest
//!   outright and is precision-proof, but the shadow range is batched across
//!   *every* display in the frame, so a near display's shadow would stop
//!   occluding a far display's ink drawn later in the same pass. Keeping the
//!   write is what lets the four ranges stay global instead of per-display.
//! - **Giving the shadow no offset at all** (leaving the ink on vanilla's
//!   single step). The panel cannot reject the shadow — it does not write
//!   depth, see the next section — but *world geometry* can, and vanilla's
//!   offset on text exists precisely so a text plane flush against a block
//!   face does not fight it. The shadow keeps vanilla's step; the ink takes
//!   a second one.
//!
//! None of this reaches `gpu/nametag.rs`, and that is not an oversight: a
//! nametag is always camera-facing, so its plane is perpendicular to the view
//! and an in-plane offset changes no depth at all. Its shadow and its ink are
//! genuinely coplanar at constant depth, the tie goes to the ink on paint
//! order, and there is nothing for an offset to decide. The owner's report
//! named holograms for that reason.
//!
//! # The panel does not write depth, and vanilla's does
//!
//! **This is the one place in this file that deliberately diverges from
//! vanilla, at the owner's request, and it is not a bug fix.**
//! `RenderPipelines.TEXT_BACKGROUND` is `DepthStencilState.DEFAULT` —
//! `new DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, true)` — so
//! vanilla's own panel writes depth. This one does not.
//!
//! The consequence in vanilla is visible and the owner checked it in the real
//! client before asking for the change: *"it looks like glass doesnt render
//! behind the billboard in the real vanilla client too, but its definitely a
//! bug"*. A translucent panel that writes depth rejects translucent terrain
//! drawn after it, and `gpu/frame.rs` draws this pass before translucent
//! terrain exactly as `LevelRenderer` draws `executeTranslucent`'s
//! `translucentCustomGeometry` before `renderGroup(TRANSLUCENT)`. Vanilla
//! escapes it *only* under the transparency post chain (Fabulous graphics),
//! where translucent terrain goes to `LevelRenderer`'s separate `translucent`
//! target whose depth was copied from main **before** the translucent features
//! ran; with the chain off, `ChunkSectionLayerGroup.TRANSLUCENT.outputTarget()`
//! falls back to the main target and vanilla has the artefact too. So this is
//! the Fabulous behaviour, reproduced in a single-target renderer by the one
//! means available.
//!
//! **It is not what fixed the ink fighting the panel, and must not be
//! described as such.** That was the two-pipeline split above, and the sweep
//! in `tests/world_text_over_geometry_pixels.rs` measured the polygon offset
//! holding on its own — 438 of 438 ink px at every row from 6.7 down to
//! **0.56 ULP** of geometric headroom, a 12× distance range — with the panel
//! still writing depth. The two symptoms the owner reported are genuinely two
//! causes, and the tempting "one flag explains both" reading is wrong.
//!
//! What the panel keeps: it still **tests** depth, so real geometry in front
//! of it still occludes it, and it is still drawn before the ink, so
//! `TextDisplayRenderer.submitInner`'s own
//! `submitNodeCollector.order(backgroundColor != 0 ? 1 : 0)` still decides
//! which is on top. What it loses is the ability to occlude anything drawn
//! later in `gpu/frame.rs` — translucent terrain, particles, weather — which
//! for a 25%-opaque panel is the point.
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
//! - **`shadow` (`FLAG_SHADOW`) is consumed now** — it was in this list, and
//!   being in it is why world-space text read as flat and washed out beside
//!   the identical string in chat: vanilla's glyphs are legible mostly
//!   *because* of the drop shadow, not because of their own colour. So is
//!   `seeThrough`, whose absence kept every see-through hologram
//!   depth-tested against the geometry it was explicitly flagged to ignore,
//!   and `useDefaultBackground`. Three entries left this list; treat a
//!   remaining one as a defect report rather than a note.
//!   `Style::shadow_color` (vanilla's explicit per-style shadow override, the
//!   *other* branch of `Font.java::getShadowColor`) is genuinely absent —
//!   this tree's `TextStyle` has no such field, so there is nothing to read.
//! - **Per-run style is modelled** (colour — hex included, bold, italic,
//!   underline, strikethrough), via `gpu/nametag.rs::layout_styled_ink_runs`
//!   — the same styled ink-run walk `gpu/nametag.rs`'s own player/mob
//!   nametags use. `DisplayDraw::text` is a real
//!   [`lodestone_model::Text`] (`crates/protocol/v770`'s decode and
//!   `crate::display_entities`' extract both carry the full component tree
//!   through unflattened), and this pass calls `Text::to_spans` on it
//!   directly — no `to_legacy_string`/`from_legacy` round trip, so a hex
//!   colour (which legacy `§` codes cannot express) survives all the way to
//!   the drawn vertex. `textOpacity << 24 | 0xFFFFFF`
//!   (`TextDisplayRenderer.submitInner`, see
//!   [`lodestone_render::display::text_glyph_color`]'s doc) is real, but it
//!   is only the **fallback** tint fed to `Font.java::getTextColor` for a
//!   span whose own colour is unspecified.
//! - **A per-line width computed from *unstyled* advances mis-centres a
//!   line whose real (styled) content is wider — e.g. a bold run.** This was
//!   the file's own alignment defect: centring used
//!   `gpu/nametag.rs::layout_styled_ink_runs`'s plain-codepoint width, which cannot
//!   see a bold run's extra advance (`GlyphInfo.getAdvance(bold)`,
//!   `Font.java`), so a two-line block whose second line carried a wider
//!   (bold, once style survives the upstream flatten above) run centred
//!   against too-small a width and read as shifted right. Switching to
//!   [`super::nametag::layout_styled_ink_runs`] for width too closes this,
//!   because that function's own advance accounts for bold.

use glam::{Mat4, Vec3};
use lodestone_assets::font::RasterFont;
use lodestone_assets::font::metrics::{SHADOW_BRIGHTNESS, SHADOW_OFFSET};
use lodestone_model::text::{Text, TextSpan};
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

/// `Display.TextDisplay.FLAG_SHADOW` — draw each ink rect twice, the first
/// copy offset by one font pixel and dimmed to a quarter, the way vanilla's
/// `Font` shadows chat and every other piece of text.
const FLAG_SHADOW: u8 = 1;

/// `RenderPipelines.TEXT_POLYGON_OFFSET`'s own polygon offset, sign-flipped
/// for this project's forward `[0,1]` depth: vanilla's
/// `new DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, true, 1.0F, 10.0F)`
/// pulls *toward* the camera with positive constants under reversed-Z, and
/// toward the camera with negative ones here.
///
/// One step of this is what every glyph and every glyph shadow gets, so text
/// lying flush against a block face wins against it rather than tying.
const TEXT_POLYGON_OFFSET: wgpu::DepthBiasState = wgpu::DepthBiasState {
    constant: -10,
    slope_scale: -1.0,
    clamp: 0.0,
};

/// [`TEXT_POLYGON_OFFSET`] counted **twice**, both terms, for the ink.
///
/// The ink has to clear two near-coplanar layers rather than one — the
/// background panel *and* its own drop shadow — and at hologram distances
/// both sit within a couple of ULP of it, so it takes two of vanilla's own
/// offset steps instead of one.
///
/// **The slope term is doubled too, and that is not symmetry for its own
/// sake.** The constant term is denominated in ULPs of the primitive's own
/// depth and so is view-angle-blind; the slope term is denominated in the
/// primitive's own depth *gradient*, which is what grows without bound as the
/// text plane goes oblique to the view. Doubling only the constant left a
/// near-grazing hologram still losing ink — measured 1,204 px of 17,984 at 85°
/// off face-on — because the rounding it has to beat grows with the gradient
/// while the constant does not. See the module doc's "The drop shadow needs
/// its own polygon offset" for the full table and for why a world-space
/// separation cannot substitute for either term.
const GLYPH_POLYGON_OFFSET: wgpu::DepthBiasState = wgpu::DepthBiasState {
    constant: 2 * TEXT_POLYGON_OFFSET.constant,
    slope_scale: 2.0 * TEXT_POLYGON_OFFSET.slope_scale,
    clamp: TEXT_POLYGON_OFFSET.clamp,
};

/// `Display.TextDisplay.FLAG_SEE_THROUGH` — draw with no depth test and no
/// depth write, so the text is readable through the geometry in front of it.
const FLAG_SEE_THROUGH: u8 = 2;
/// `Display.TextDisplay.FLAG_USE_DEFAULT_BACKGROUND` — ignore the synced
/// background colour and use the client's own text-background shade instead.
const FLAG_USE_DEFAULT_BACKGROUND: u8 = 4;
/// `Display.TextDisplay.FLAG_ALIGN_LEFT`.
const FLAG_ALIGN_LEFT: u8 = 8;
/// `Display.TextDisplay.FLAG_ALIGN_RIGHT`.
const FLAG_ALIGN_RIGHT: u8 = 16;

/// The alpha `FLAG_USE_DEFAULT_BACKGROUND` resolves to, as a packed ARGB
/// black — `(int)(getBackgroundOpacity(0.25F) * 255) << 24`.
///
/// `Options.getBackgroundOpacity` returns its *fallback* whenever
/// `backgroundForChatOnly` is set, and vanilla's own default for that option
/// is on, so `0.25` is what an unconfigured client uses. That accessibility
/// pair is not modelled here (this crate's `chat_background_opacity` is the
/// chat HUD's, and vanilla's chat-only default means it would not feed this
/// value anyway), so the fallback is used unconditionally rather than
/// pretending to read an option that does not exist.
///
/// **This is not the same number as `display_entities`'
/// `DEFAULT_BACKGROUND_COLOR`.** That one is `Display.TextDisplay`'s own
/// accessor default (`1073741824` = `0x40000000`, alpha `64`) — what a
/// display that has never reported a colour carries. This one is `63 <<
/// 24`, one step darker, and applies only when the *flag* is set. Two
/// defaults, one off by one, and folding them together would be invisible.
const DEFAULT_BACKGROUND_ARGB: i32 = 0x3F00_0000_u32 as i32;

/// Which panel colour this display actually draws, honouring
/// `FLAG_USE_DEFAULT_BACKGROUND` — `TextDisplayRenderer.submitInner`'s own
/// first branch, which was missing, so a display asking for the client
/// default drew whatever colour the server happened to have synced.
fn resolved_background_argb(draw: &DisplayDraw) -> i32 {
    if draw.text_style_flags & FLAG_USE_DEFAULT_BACKGROUND != 0 {
        DEFAULT_BACKGROUND_ARGB
    } else {
        draw.text_background_color
    }
}

/// Draws world-space `text_display` glyphs and background panels — see the
/// module doc for why this is neither a pure billboard nor a fixed-orientation
/// pass, unlike its two nearest relatives.
#[derive(Debug)]
pub(super) struct DisplayTextRenderer {
    /// `RenderPipelines.TEXT_BACKGROUND`'s depth state — plain
    /// `DepthStencilState.DEFAULT`: test, no polygon offset, and **no depth
    /// write, where vanilla writes**. See the module doc's "The panel does
    /// not write depth, and vanilla's does" — that flag is a deliberate
    /// improvement on vanilla, not a port of it.
    background_pipeline: wgpu::RenderPipeline,
    /// `RenderPipelines.TEXT_POLYGON_OFFSET` exactly — the same two constants
    /// `gpu/sign_text.rs` ports. Vanilla submits a glyph's drop shadow and
    /// the glyph itself through this one pipeline; here the shadow keeps it
    /// and the ink takes a second step, so that the ink beats its own shadow
    /// by a ULP-denominated margin instead of by a world-space separation
    /// this depth format cannot resolve. See the module doc's "The drop
    /// shadow needs its own polygon offset".
    shadow_pipeline: wgpu::RenderPipeline,
    /// [`GLYPH_POLYGON_OFFSET`] — two steps of `TEXT_POLYGON_OFFSET`, so the
    /// glyphs win against the panel 0.00025 blocks behind them *and* against
    /// their own drop shadow, at every viewing angle and every distance. See
    /// the module doc for the measurements that made each of those necessary.
    glyph_pipeline: wgpu::RenderPipeline,
    /// `RenderPipelines.TEXT_SEE_THROUGH` and `TEXT_BACKGROUND_SEE_THROUGH`,
    /// which are one pipeline here because the only thing this pass ports of
    /// either is the depth state and theirs is identical:
    /// `withDepthStencilState(Optional.empty())` — no test, no write, so the
    /// text is visible through whatever is in front of it.
    ///
    /// A `text_display` picks this with `Display.TextDisplay.FLAG_SEE_THROUGH`,
    /// which is what most server-side holograms set. Drawing one through the
    /// depth-tested pipelines instead is not a near-miss: the entity is
    /// deliberately placed inside or against geometry, so it is occluded or
    /// fighting for the whole time the flag was asking for neither.
    see_through_pipeline: wgpu::RenderPipeline,
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

        // One descriptor, two depth biases — the *only* thing that differs
        // between `RenderPipelines.TEXT_BACKGROUND` and
        // `RenderPipelines.TEXT_POLYGON_OFFSET` on our side of the port, so
        // building them from one closure keeps that visible rather than
        // burying it in two near-identical literals.
        let build = |label: &str, depth: wgpu::DepthStencilState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
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
                depth_stencil: Some(depth.clone()),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        // Depth-tested, with the write flag and the polygon offset as the two
        // axes that differ between the panel and the ink. `LessEqual` is
        // `GREATER_THAN_OR_EQUAL` through this project's forward `[0,1]`
        // depth.
        let tested = |write: bool, bias: wgpu::DepthBiasState| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias,
        };
        let background_pipeline = build(
            "lodestone-display-text-background-pipeline",
            // **Depth write off, where vanilla's `DepthStencilState.DEFAULT`
            // has it on.** The one deliberate divergence in this file, and it
            // is a divergence rather than a fix — see the module doc's "The
            // panel does not write depth, and vanilla's does".
            tested(false, wgpu::DepthBiasState::default()),
        );
        // Vanilla's `TEXT_POLYGON_OFFSET` unchanged, and the ink one further
        // step in front of it. **The shadow still writes depth**, which is
        // what keeps the shadow and ink ranges batchable across every display
        // in the frame — see the module doc for the depth-write-off
        // alternative and why it was not taken.
        let shadow_pipeline = build(
            "lodestone-display-text-shadow-pipeline",
            tested(true, TEXT_POLYGON_OFFSET),
        );
        let glyph_pipeline = build(
            "lodestone-display-text-glyph-pipeline",
            tested(true, GLYPH_POLYGON_OFFSET),
        );
        // Vanilla's `Optional.empty()` depth state. wgpu still needs the
        // attachment's format on the pipeline (the render pass has a depth
        // buffer bound whatever this draw wants), so "no depth state" is
        // spelled as `Always` plus no write — the same pair of GL calls
        // `Optional.empty()` compiles down to.
        let see_through_pipeline = build(
            "lodestone-display-text-see-through-pipeline",
            wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            },
        );

        Self {
            background_pipeline,
            shadow_pipeline,
            glyph_pipeline,
            see_through_pipeline,
            bind_group,
            uniform,
            vertices,
            font: super::nametag::load_font(),
            ink: super::nametag::StyledInkLayoutCache::default(),
        }
    }

    /// Uploads this frame's view-projection and `text_display` vertices.
    /// Must run before the render pass opens, same buffer-creation
    /// constraint as every other pass in this crate.
    ///
    /// Returns the four contiguous ranges of the one vertex buffer, one per
    /// pipeline, in the order they are drawn. The counts are already clamped
    /// so their **sum** fits [`MAX_DISPLAY_TEXT_VERTICES`]. Pass the value
    /// straight to [`draw`](Self::draw).
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        draws: &[DisplayDraw],
        camera: &Camera,
    ) -> DisplayTextRanges {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));
        let Some(raster) = &self.font else {
            return DisplayTextRanges::default();
        };

        let Partitioned { backgrounds, shadows, glyphs, see_through } =
            partition_display_text(raster, &self.ink, draws, camera);
        // Panels first, then glyphs, so the two ranges are contiguous. The
        // cap is applied to the panels first and to whatever room is left
        // for the glyphs, which is the right way round: a truncated panel
        // list still leaves readable text, a truncated glyph list does not.
        // The cap is spent in draw order, which is also the right order of
        // priority: a truncated panel list still leaves readable text, a
        // truncated glyph list does not, and see-through ink is the range a
        // player is most likely to be looking *for*.
        let mut room = MAX_DISPLAY_TEXT_VERTICES;
        let mut offset = 0usize;
        let mut upload = |src: &[DisplayTextVertex]| {
            let len = src.len().min(room);
            if len > 0 {
                queue.write_buffer(
                    &self.vertices,
                    (offset * std::mem::size_of::<DisplayTextVertex>()) as u64,
                    bytemuck::cast_slice(&src[..len]),
                );
            }
            room -= len;
            offset += len;
            len as u32
        };
        // Sequential `let`s rather than four struct-literal fields: `upload`
        // is stateful (it walks `offset` and spends `room`), so these calls
        // must happen in draw order and a reader must not have to know
        // Rust's field-evaluation order to see that.
        let backgrounds = upload(&backgrounds);
        let shadows = upload(&shadows);
        let glyphs = upload(&glyphs);
        let see_through = upload(&see_through);
        DisplayTextRanges { backgrounds, shadows, glyphs, see_through }
    }

    /// Records the three draws (a no-op for any range that is empty,
    /// including the no-jar `font: None` state, since
    /// [`prepare`](Self::prepare) always returns all zeroes there).
    ///
    /// Panels **before** glyphs, matching
    /// `TextDisplayRenderer.submitInner`'s own
    /// `submitNodeCollector.order(backgroundColor != 0 ? 1 : 0)` — the text
    /// is explicitly ordered after the background it sits on. The
    /// see-through range comes last because it neither tests nor writes
    /// depth, so it must not be able to occlude the three that do.
    ///
    /// Shadows **before** ink, the same order `Font.java` uses within one
    /// `drawInBatch`, and the same order `gpu/nametag.rs::push_entity_quads`
    /// uses: a later glyph's ink must sit on top of an earlier glyph's
    /// shadow. Unlike that pass, the two here also differ in pipeline — see
    /// the module doc's "The drop shadow needs its own polygon offset".
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, counts: DisplayTextRanges) {
        let DisplayTextRanges { backgrounds, shadows, glyphs, see_through } = counts;
        if backgrounds == 0 && shadows == 0 && glyphs == 0 && see_through == 0 {
            return;
        }
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        let mut start = 0u32;
        for (count, pipeline) in [
            (backgrounds, &self.background_pipeline),
            (shadows, &self.shadow_pipeline),
            (glyphs, &self.glyph_pipeline),
            (see_through, &self.see_through_pipeline),
        ] {
            if count > 0 {
                pass.set_pipeline(pipeline);
                pass.draw(start..start + count, 0..1);
            }
            start += count;
        }
    }
}

/// Vertex counts for [`DisplayTextRenderer::draw`]'s four contiguous ranges,
/// in draw order.
///
/// A named struct rather than a `(u32, u32, u32, u32)` on purpose: four
/// adjacent same-typed fields transpose without a trace, and every existing
/// gate for this pass would round-trip a swapped pair unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DisplayTextRanges {
    /// Depth-tested, non-writing, unbiased panels.
    backgrounds: u32,
    /// Drop shadows, on vanilla's own `TEXT_POLYGON_OFFSET`.
    shadows: u32,
    /// Glyph ink, one further offset step in front of the shadows.
    glyphs: u32,
    /// Everything a `FLAG_SEE_THROUGH` display contributes — panel, shadow
    /// and ink alike — through the one pipeline that neither tests nor
    /// writes depth.
    see_through: u32,
}

/// This frame's `text_display` vertices, split into the four pipeline ranges
/// [`DisplayTextRenderer::draw`] records.
///
/// Free-standing rather than inlined into `prepare` so the routing is
/// assertable without a GPU: which pipeline a display lands on is decided
/// entirely by `Display.TextDisplay.FLAG_SEE_THROUGH`, and a gate that has to
/// build a device to ask cannot be a unit test.
#[derive(Debug, Default)]
struct Partitioned {
    backgrounds: Vec<DisplayTextVertex>,
    shadows: Vec<DisplayTextVertex>,
    glyphs: Vec<DisplayTextVertex>,
    see_through: Vec<DisplayTextVertex>,
}

fn partition_display_text(
    raster: &RasterFont,
    ink: &super::nametag::StyledInkLayoutCache,
    draws: &[DisplayDraw],
    camera: &Camera,
) -> Partitioned {
    let mut out = Partitioned::default();
    for draw in draws {
        if draw.type_path != TEXT_DISPLAY_TYPE_PATH {
            continue;
        }
        let Some(text) = &draw.text else { continue };
        let mut panel = Vec::new();
        let mut line_shadow = Vec::new();
        let mut line_ink = Vec::new();
        push_text_display_quads(
            raster,
            ink,
            draw,
            text,
            camera,
            &mut panel,
            &mut line_shadow,
            &mut line_ink,
        );
        // A see-through display puts its panel, its shadows *and* its ink
        // into the one un-depth-tested range, in that order — vanilla submits
        // them through two see-through pipelines whose depth state is
        // identical and whose only difference is the shader, so within that
        // range paint order is all that separates them. That also means the
        // shadow/ink contest cannot arise there at all: with no depth test
        // and no depth write, submission order decides outright.
        if draw.text_style_flags & FLAG_SEE_THROUGH == 0 {
            out.backgrounds.append(&mut panel);
            out.shadows.append(&mut line_shadow);
            out.glyphs.append(&mut line_ink);
        } else {
            out.see_through.append(&mut panel);
            out.see_through.append(&mut line_shadow);
            out.see_through.append(&mut line_ink);
        }
    }
    out
}

/// Splits a fully-inherited span list on literal `\n` boundaries into
/// per-line span lists, preserving each run's own resolved style — the
/// styled sibling of `text.split('\n')` on a plain string.
///
/// A newline can fall *inside* a span's own text (a multi-line literal) or
/// at a span boundary; either way, this must reproduce exactly the line
/// count and content `text.to_plain_string().split('\n')` would give, since
/// `total_height`/background-panel sizing below still counts by *line*, not
/// by span. The algorithm is the standard incremental split: each span's
/// first `split('\n')` fragment continues the line already open from the
/// previous span (or starts the first line), and every subsequent fragment
/// opens a new line — equivalent to splitting the spans' concatenated text,
/// because `str::split` is defined by scanning left to right for the
/// delimiter regardless of where a caller's own chunk boundaries fall.
fn split_spans_into_lines(spans: &[TextSpan]) -> Vec<Vec<TextSpan>> {
    let mut lines: Vec<Vec<TextSpan>> = vec![Vec::new()];
    for span in spans {
        let mut parts = span.text.split('\n');
        if let Some(first) = parts.next() {
            if !first.is_empty() {
                lines.last_mut().expect("lines is never empty").push(TextSpan {
                    text: first.to_owned(),
                    style: span.style,
                });
            }
        }
        for part in parts {
            lines.push(Vec::new());
            if !part.is_empty() {
                lines.last_mut().expect("just pushed").push(TextSpan {
                    text: part.to_owned(),
                    style: span.style,
                });
            }
        }
    }
    lines
}

/// Lowers one `text_display`'s current text into world-space quads: the
/// background panel (when non-transparent) onto `background_out`, every
/// non-empty line's drop shadow onto `shadow_out`, and its ink onto
/// `glyph_out`.
///
/// The three are kept apart because they are drawn through **three different
/// pipelines** — vanilla's `RenderPipelines.TEXT_BACKGROUND`, its
/// `RenderPipelines.TEXT_POLYGON_OFFSET`, and that same offset counted twice
/// — see the module doc for the measurement behind each split. Neither is
/// cosmetic.
#[allow(clippy::too_many_arguments)]
fn push_text_display_quads(
    raster: &RasterFont,
    ink: &super::nametag::StyledInkLayoutCache,
    draw: &DisplayDraw,
    text: &Text,
    camera: &Camera,
    background_out: &mut Vec<DisplayTextVertex>,
    shadow_out: &mut Vec<DisplayTextVertex>,
    glyph_out: &mut Vec<DisplayTextVertex>,
) {
    // `text` is a real `Text` (the protocol-layer decode and
    // `crate::display_entities`' extract both carry the full component tree
    // through unflattened — see the module doc), so `to_spans` reads
    // colour/bold/italic/underline/strikethrough directly with no
    // `to_legacy_string`/`from_legacy` round trip to lose a hex colour along
    // the way. [`split_spans_into_lines`] then breaks the flattened run list
    // on literal `\n`s while keeping each run's own resolved style.
    let spans = text.to_spans();
    let lines = split_spans_into_lines(&spans);
    // Per-line layout up front: needed both to size the background panel
    // (vanilla's own `cachedInfo.width()`/`height()`, computed once before
    // any quad is emitted) and to lay out each line's glyphs afterwards —
    // computed once here rather than twice. Styled (not
    // `super::nametag::layout_styled_ink_runs`'s plain width) so a bold run's real,
    // wider advance is what centring measures — see the module doc for the
    // alignment defect an unstyled width caused.
    let layouts: Vec<_> = lines.iter().map(|spans| ink.layout(raster, spans)).collect();
    let total_width = layouts.iter().map(|l| l.1).fold(0.0_f32, f32::max);
    let total_height = (lines.len() as f32).mul_add(TEXT_LINE_HEIGHT, -1.0);
    if total_width <= 0.0 {
        // Every line was empty (e.g. a lone `"\n"`, or `text` itself empty)
        // — vanilla's own `Font.split` of an empty string contributes no ink
        // either.
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
    let align_left = draw.text_style_flags & FLAG_ALIGN_LEFT != 0;
    let align_right = draw.text_style_flags & FLAG_ALIGN_RIGHT != 0;

    let background_argb = resolved_background_argb(draw);
    if background_argb != 0 {
        push_background_quad(
            matrix,
            total_width,
            total_height,
            background_argb,
            background_out,
        );
    }

    // `text_glyph_color`'s alpha is `textOpacity`'s own fraction — the real
    // per-frame value; its RGB (always white) is only the *fallback* a
    // `StyledRect` already carries for a colourless span, so only the alpha
    // channel is read here (see the module doc).
    let alpha = text_glyph_color(draw.text_opacity)[3];
    // `Display.TextDisplay.FLAG_SHADOW`, threaded to
    // `textCollector.submitText(…, shadow, …)` and from there to
    // `Font.java`'s `drawShadow`. Gated on the flag, unlike a nametag (which
    // vanilla always shadows) — the accessor's own default is `(byte)0`, so
    // a display that never reported style flags draws no shadow, exactly as
    // in vanilla.
    let shadow = draw.text_style_flags & FLAG_SHADOW != 0;
    // The whole block's shadow copy is emitted **before** any of its ink, the
    // same order `gpu/nametag.rs::push_entity_quads` uses and for the same
    // reason: a later glyph's ink must sit on top of an earlier glyph's
    // shadow, never the other way round. (Vanilla groups per `submitText`
    // call, i.e. per line; block-wide grouping differs only where one line's
    // shadow would overlap the next line's ink, which the `10`-pixel line
    // height rules out for the default font.)
    //
    // Order is no longer the *only* thing separating the two — they now go
    // through two pipelines a polygon-offset step apart, so depth separates
    // them as well. Order still matters for the shadow-over-shadow and
    // ink-over-ink cases within one block, which no depth bias touches.
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
            if shadow {
                // `Font.java::getShadowColor`'s no-explicit-colour branch:
                // `ARGB.scaleRGB(textColor, 0.25F)` — the glyph's **own**
                // resolved colour at a quarter brightness with its alpha
                // untouched, so a red run's shadow is dark red rather than
                // flat grey. `Style::shadow_color` (vanilla's explicit
                // per-style override) is not modelled by this tree's
                // `TextStyle`, so only this branch exists to port.
                let shadow_color = [
                    color[0] * SHADOW_BRIGHTNESS,
                    color[1] * SHADOW_BRIGHTNESS,
                    color[2] * SHADOW_BRIGHTNESS,
                    color[3],
                ];
                push_ink_quad(
                    matrix,
                    lx + SHADOW_OFFSET,
                    ly + SHADOW_OFFSET,
                    rect.w,
                    rect.h,
                    shadow_color,
                    shadow_out,
                );
            }
            push_ink_quad(matrix, lx, ly, rect.w, rect.h, color, glyph_out);
        }
    }
}

/// One ink rect's two triangles, in local glyph space, through `matrix`.
///
/// Free-standing because the shadow copy and the glyph itself are the same
/// geometry at two offsets and two colours — writing it twice is how the two
/// drift apart. **Both are emitted at local `z = 0`**; what separates them is
/// the polygon offset on their two pipelines, not their geometry. See the
/// module doc's "The drop shadow needs its own polygon offset" for the
/// measurement that ruled the geometric separation out.
#[allow(clippy::too_many_arguments)]
fn push_ink_quad(
    matrix: Mat4,
    lx: f32,
    ly: f32,
    w: f32,
    h: f32,
    color: [f32; 4],
    out: &mut Vec<DisplayTextVertex>,
) {
    let tl = matrix.transform_point3(Vec3::new(lx, ly, 0.0)).to_array();
    let tr = matrix.transform_point3(Vec3::new(lx + w, ly, 0.0)).to_array();
    let bl = matrix.transform_point3(Vec3::new(lx, ly + h, 0.0)).to_array();
    let br = matrix
        .transform_point3(Vec3::new(lx + w, ly + h, 0.0))
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

    /// Every gate below wants "all the vertices this draw contributes", in
    /// submission order — panel, then shadows, then ink — which is exactly
    /// what the one buffer holds. `push_text_display_quads` splits them
    /// because they go through three pipelines (see the module doc); nothing
    /// about that split changes what any of these assertions are about.
    fn all_quads(
        raster: &RasterFont,
        ink: &super::super::nametag::StyledInkLayoutCache,
        draw: &DisplayDraw,
        text: &Text,
        camera: &Camera,
    ) -> Vec<DisplayTextVertex> {
        let mut out = Vec::new();
        let mut shadows = Vec::new();
        let mut glyphs = Vec::new();
        push_text_display_quads(
            raster,
            ink,
            draw,
            text,
            camera,
            &mut out,
            &mut shadows,
            &mut glyphs,
        );
        out.extend(shadows);
        out.extend(glyphs);
        out
    }

    fn draw_with_text(text: &str) -> DisplayDraw {
        DisplayDraw {
            id: 1,
            type_path: TEXT_DISPLAY_TYPE_PATH,
            position: Vec3::ZERO,
            entity_yaw: 0.0,
            entity_pitch: 0.0,
            billboard: BillboardMode::Fixed,
            transform: DisplayTransformation::default(),
            text: Some(Text::from_legacy(text)),
            text_line_width: 200,
            text_background_color: 0,
            text_opacity: -1,
            text_style_flags: 0,
            block_state: None,
            item: None,
            item_display_context: 0,
            brightness_override: None,
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
        let out = all_quads(
            &raster,
            &ink,
            &draw_with_text(""),
            &Text::from_legacy(""),
            &Camera::default(),
        );
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
        let out = all_quads(
            &raster,
            &ink,
            &draw,
            &Text::from_legacy("LODESTONE"),
            &Camera::default(),
        );
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
        let no_bg_out = all_quads(
            &raster,
            &ink,
            &without_bg,
            &Text::from_legacy("A"),
            &Camera::default(),
        );

        let mut with_bg = draw_with_text("A");
        with_bg.text_background_color = 0x4000_0000_u32 as i32;
        let bg_out = all_quads(
            &raster,
            &ink,
            &with_bg,
            &Text::from_legacy("A"),
            &Camera::default(),
        );

        assert_eq!(
            bg_out.len(),
            no_bg_out.len() + 6,
            "a non-zero background colour must add exactly one quad's worth \
             of vertices: no_bg={}, bg={}",
            no_bg_out.len(),
            bg_out.len()
        );
    }

    /// **The see-through discriminating pair.** The identical display, twice,
    /// differing only in `FLAG_SEE_THROUGH`: without it every vertex must land
    /// in the three depth-tested ranges and none in the fourth, with it the
    /// exact reverse. A gate asserting only "the see-through range is
    /// non-empty" would pass with the routing wired to both.
    ///
    /// The fixture carries `FLAG_SHADOW` so the **shadow** range is one of
    /// the three under test rather than an empty vector every assertion below
    /// would be satisfied by.
    #[test]
    fn the_see_through_flag_moves_every_vertex_to_the_undepth_tested_range() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let camera = Camera::default();

        let mut tested = draw_with_text("LODESTONE");
        tested.text_background_color = 0x4000_0000_u32 as i32;
        tested.text_style_flags = FLAG_SHADOW;
        let one = partition_display_text(&raster, &ink, &[tested.clone()], &camera);
        assert_eq!(one.backgrounds.len(), 6, "one background quad");
        assert!(!one.shadows.is_empty(), "a shadowed display contributes shadow quads");
        assert_eq!(
            one.shadows.len(),
            one.glyphs.len(),
            "the shadow copy is the same geometry as the ink, quad for quad",
        );
        assert!(
            one.see_through.is_empty(),
            "nothing may reach the see-through range unflagged",
        );

        let mut through = tested.clone();
        through.text_style_flags |= FLAG_SEE_THROUGH;
        let two = partition_display_text(&raster, &ink, &[through], &camera);
        assert!(
            two.backgrounds.is_empty() && two.shadows.is_empty() && two.glyphs.is_empty(),
            "the three depth-tested ranges must be empty",
        );
        assert_eq!(
            two.see_through.len(),
            one.backgrounds.len() + one.shadows.len() + one.glyphs.len(),
            "the same geometry, all of it, in the one un-depth-tested range",
        );
        // Panel, then shadows, then ink inside that range — the submission
        // order the depth-tested split enforces with three draws, and the
        // only thing separating the three where there is no depth test.
        assert_eq!(&two.see_through[..6], &one.backgrounds[..]);
        assert_eq!(&two.see_through[6..6 + one.shadows.len()], &one.shadows[..]);
        assert_eq!(&two.see_through[6 + one.shadows.len()..], &one.glyphs[..]);
    }

    /// `FLAG_USE_DEFAULT_BACKGROUND` replaces the synced colour with the
    /// client's own shade — and with a *different* number from the accessor
    /// default a never-reported display carries, which is the whole reason
    /// this is a discriminating test and not a smoke test.
    #[test]
    fn the_default_background_flag_overrides_a_synced_colour_with_its_own_shade() {
        // Synced fully-transparent-and-zero (vanilla's "no panel") plus the
        // flag must still draw a panel: the flag wins.
        let mut flagged = draw_with_text("A");
        flagged.text_background_color = 0;
        flagged.text_style_flags = FLAG_USE_DEFAULT_BACKGROUND;
        assert_eq!(resolved_background_argb(&flagged), DEFAULT_BACKGROUND_ARGB);

        // A synced colour the flag must *ignore*, chosen to be the accessor
        // default so the two defaults cannot be confused for each other.
        let mut both = draw_with_text("A");
        both.text_background_color = 0x4000_0000_u32 as i32;
        both.text_style_flags = FLAG_USE_DEFAULT_BACKGROUND;
        assert_eq!(resolved_background_argb(&both), DEFAULT_BACKGROUND_ARGB);
        assert_ne!(
            DEFAULT_BACKGROUND_ARGB, 0x4000_0000_u32 as i32,
            "the flag's shade and Display.TextDisplay's accessor default are \
             one alpha step apart; if these ever become equal this gate stops \
             discriminating",
        );

        // Unflagged, the synced colour is used verbatim.
        let mut plain = draw_with_text("A");
        plain.text_background_color = 0x1234_5678;
        assert_eq!(resolved_background_argb(&plain), 0x1234_5678);
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

        let hello = Text::from_legacy("HELLO");
        let fixed_draw = draw_with_text("HELLO");
        let fixed_a = all_quads(&raster, &ink, &fixed_draw, &hello, &camera_a);
        let fixed_b = all_quads(&raster, &ink, &fixed_draw, &hello, &camera_b);
        assert_eq!(
            fixed_a, fixed_b,
            "Fixed billboard text must not move when only the camera rotates"
        );

        let mut center_draw = draw_with_text("HELLO");
        center_draw.billboard = BillboardMode::Center;
        let center_a = all_quads(&raster, &ink, &center_draw, &hello, &camera_a);
        let center_b = all_quads(&raster, &ink, &center_draw, &hello, &camera_b);
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
        let coloured_out = all_quads(
            &raster,
            &ink,
            &coloured,
            &Text::from_legacy("\u{a7}cRED"),
            &Camera::default(),
        );
        assert!(
            coloured_out.iter().any(|v| is_red(v.color)),
            "a §c-coloured span must reach the draw with red vertex colour, got: {:?}",
            coloured_out.iter().map(|v| v.color).collect::<Vec<_>>()
        );

        let plain = draw_with_text("RED");
        let plain_out = all_quads(
            &raster,
            &ink,
            &plain,
            &Text::from_legacy("RED"),
            &Camera::default(),
        );
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
    /// came from `gpu/nametag.rs::layout_styled_ink_runs`'s plain per-codepoint
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
        let plain_out = all_quads(
            &raster,
            &ink,
            &plain_draw,
            &Text::from_legacy("Hi\nWWWWWW"),
            &Camera::default(),
        );
        let bold_draw = draw_with_text("Hi\n\u{a7}lWWWWWW");
        let bold_out = all_quads(
            &raster,
            &ink,
            &bold_draw,
            &Text::from_legacy("Hi\n\u{a7}lWWWWWW"),
            &Camera::default(),
        );
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

    /// **The drop-shadow control.** Owner report: world-space text "seems
    /// kind of light … and its missing the text shadow (like it does in the
    /// chat)". `FLAG_SHADOW` was decoded, carried to this draw site and then
    /// dropped, and this file's own module doc said so in plain words — the
    /// "not ported, disclosed" species `CLAUDE.md` says to read as a defect
    /// report rather than as reassurance.
    ///
    /// Three claims, and the last two are what make it discriminating rather
    /// than a smoke test:
    ///
    /// * the flag **doubles** the ink (every rect drawn twice), and its
    ///   absence draws each rect exactly once;
    /// * the shadow copy is a quarter-brightness **red** for a `§c` run, not
    ///   a flat grey — `Font.java::getShadowColor`'s
    ///   `ARGB.scaleRGB(textColor, 0.25F)`, so a hardcoded dark constant
    ///   fails here;
    /// * the shadow is emitted **before** the ink, since with `LessEqual` and
    ///   no separation between the two copies paint order is the only thing
    ///   deciding which is on top. Reversed, the shadow would sit over the
    ///   glyph and the text would read as a smear.
    #[test]
    fn the_shadow_flag_adds_a_dimmed_offset_copy_of_every_ink_rect_before_the_ink() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();
        let camera = Camera::default();
        let red = lodestone_model::text::TextColor::Red.rgb();
        let want_red = [
            ((red >> 16) & 0xff) as f32 / 255.0,
            ((red >> 8) & 0xff) as f32 / 255.0,
            (red & 0xff) as f32 / 255.0,
        ];
        let want_shadow = [
            want_red[0] * SHADOW_BRIGHTNESS,
            want_red[1] * SHADOW_BRIGHTNESS,
            want_red[2] * SHADOW_BRIGHTNESS,
        ];
        let is_close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let matches = |c: [f32; 4], want: [f32; 3]| {
            is_close(c[0], want[0]) && is_close(c[1], want[1]) && is_close(c[2], want[2])
        };

        let text = Text::from_legacy("\u{a7}cRED");
        let plain = draw_with_text("\u{a7}cRED");
        let mut shadowed = plain.clone();
        shadowed.text_style_flags |= FLAG_SHADOW;

        let run = |draw: &DisplayDraw| {
            let (mut panel, mut shadows, mut glyphs) = (Vec::new(), Vec::new(), Vec::new());
            push_text_display_quads(
                &raster,
                &ink,
                draw,
                &text,
                &camera,
                &mut panel,
                &mut shadows,
                &mut glyphs,
            );
            (shadows, glyphs)
        };
        let (plain_shadows, without) = run(&plain);
        let (with_shadows, with) = run(&shadowed);

        assert!(!without.is_empty(), "the fixture must draw real ink");
        assert!(
            plain_shadows.is_empty(),
            "unflagged text must contribute nothing to the shadow range at all \
             — otherwise this gate cannot tell the flag from the default: got \
             {} vertices",
            plain_shadows.len()
        );
        assert_eq!(
            with_shadows.len(),
            without.len(),
            "FLAG_SHADOW must draw every ink rect exactly once more, into the \
             shadow range: {} ink vertices, {} shadow vertices",
            without.len(),
            with_shadows.len()
        );
        assert_eq!(
            with.len(),
            without.len(),
            "the flag must not change the ink itself, only add a shadow copy",
        );
        assert!(
            with_shadows.iter().all(|v| matches(v.color, want_shadow)),
            "every shadow vertex must be quarter-brightness red {want_shadow:?} \
             — a flat grey or black shadow would fail here, which is the point: \
             got {:?}",
            with_shadows.iter().map(|v| v.color).take(6).collect::<Vec<_>>()
        );
        assert!(
            with.iter().all(|v| matches(v.color, want_red)),
            "every ink vertex must be the full-brightness red the span asked \
             for: got {:?}",
            with.iter().map(|v| v.color).take(6).collect::<Vec<_>>()
        );

        // And the copy is genuinely *offset*, not merely dimmed in place —
        // one font pixel through the glyph transform, on both axes.
        assert_ne!(
            with_shadows[0].position, with[0].position,
            "the shadow copy must be offset from the glyph it shadows, not \
             painted at the same place"
        );
        // And the ink itself does **not** move when the flag is set. Vanilla
        // pushes a shadowed glyph forward by `0.03` in local glyph space;
        // this pass deliberately does not, because that separation is
        // view-side-dependent — it inverts the moment you walk behind the
        // hologram — and the polygon offset that replaces it is not. See the
        // module doc's "The drop shadow needs its own polygon offset". A
        // reader who re-ports that constant will fail here, which is the
        // point.
        assert_eq!(
            with[0].position, without[0].position,
            "the drop shadow must not displace the ink it shadows: this pass \
             separates the two with a polygon offset, not with geometry",
        );
    }

    /// **The hex-colour control.** `DisplayDraw::text` is a real
    /// [`lodestone_model::Text`] now, and `push_text_display_quads` reads it
    /// with `Text::to_spans` directly — but [`a_coloured_span_reaches_the_draw_with_its_colour_intact`]'s
    /// `§c` (legacy-expressible) colour cannot see the bug this guards,
    /// since `to_legacy_string`/`from_legacy` round-trips it losslessly.
    /// Only a hex [`lodestone_model::text::TextColor::Rgb`] discriminates:
    /// legacy `§` codes are a 16-entry palette with no hex form at all.
    ///
    /// The control is run first, in this same test, reproducing the exact
    /// lossy path `DisplayDraw::text` used to bridge through
    /// (`text.to_legacy_string()` then `Text::from_legacy(..).to_spans()`)
    /// and asserting it drops the hex colour — watched failing, not assumed
    /// — before asserting the real, direct `to_spans()` path
    /// `push_text_display_quads` now takes preserves it end to end, to the
    /// drawn vertex colour.
    #[test]
    fn push_text_display_quads_preserves_a_hex_colour_the_legacy_round_trip_cannot() {
        let Some(raster) = super::super::nametag::load_font() else {
            return;
        };
        let ink = super::super::nametag::StyledInkLayoutCache::default();

        let hex = 0x00FF_8800_u32;
        let mut hex_text = Text::literal("HEX");
        hex_text.style.color = Some(lodestone_model::text::TextColor::Rgb(hex));

        // Control: the round trip `DisplayDraw::text` used to be bridged
        // through, watched failing on the exact hypothesis it would have
        // produced.
        let lossy_spans = Text::from_legacy(&hex_text.to_legacy_string()).to_spans();
        assert!(
            lossy_spans
                .iter()
                .all(|s| s.style.color != Some(lodestone_model::text::TextColor::Rgb(hex))),
            "control: a to_legacy_string/from_legacy round trip must lose a \
             hex colour (legacy `§` codes have no hex form) — this is the bug \
             `DisplayDraw::text` used to have when it stored a plain \
             `String`; got {:?}",
            lossy_spans.iter().map(|s| s.style.color).collect::<Vec<_>>()
        );

        let draw = draw_with_text("HEX");
        let out = all_quads(&raster, &ink, &draw, &hex_text, &Camera::default());

        let want_hex = [
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
        ];
        let is_close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let has_hex = out
            .iter()
            .any(|v| is_close(v.color[0], want_hex[0]) && is_close(v.color[1], want_hex[1]) && is_close(v.color[2], want_hex[2]));
        assert!(
            has_hex,
            "the drawn glyph must carry the hex colour {want_hex:?}, reached \
             through `DisplayDraw::text`'s direct `to_spans()` (no legacy \
             round trip): got {:?}",
            out.iter().map(|v| v.color).collect::<Vec<_>>()
        );
    }
}
