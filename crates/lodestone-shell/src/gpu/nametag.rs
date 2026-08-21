//! World-space entity/player nametags: billboarded text above
//! every entity with a visible custom name, and above every other player
//! (whose tag is always its tab-list display name — see
//! `crate::net::entity_snapshot`'s doc for exactly which vanilla rule governs
//! each case, jar file:line cited).
//!
//! # The two depth passes, and the sign flip
//!
//! Reconciled against the real 26.2 client
//! (`.cache/mc/26.2/client-src/net/minecraft/client/renderer/rendertype/RenderPipelines.java`
//! and `RenderTypes.java`), not guessed:
//!
//! * **Normal pass** (`RenderPipelines.TEXT`, via `WORLD_TEXT_SNIPPET`):
//!   `DepthStencilState.DEFAULT = new DepthStencilState(CompareOp.GREATER_THAN_OR_EQUAL, true)`
//!   (`DepthStencilState.java`) — depth-tested **and depth-written**.
//!   Vanilla's reversed-Z convention makes "closer" `GREATER_THAN_OR_EQUAL`;
//!   ours is `[0,1]` DirectX-style (`docs/`/`CLAUDE.md`'s rendering
//!   constraints), so the *sign flips* to [`wgpu::CompareFunction::LessEqual`]
//!   with `depth_write_enabled: true` — the same flip `gpu/outline.rs` and
//!   `gpu/debug_lines.rs` already apply, just with write turned **on** here
//!   (a nearer tag's glyphs must win over a farther, overlapping one, exactly
//!   as vanilla's write-enabled pass does).
//! * **See-through pass** (`RenderPipelines.TEXT_SEE_THROUGH`):
//!   `.withDepthStencilState(Optional.empty())` (`RenderPipelines.java`)
//!   — **no depth attachment use at all**, neither tested nor written. There
//!   is no comparison operator to port here, so there is no sign to get
//!   backwards — but `wgpu` itself has no "this pipeline ignores the pass's
//!   depth attachment" option: every pipeline drawn inside a render pass
//!   that has a depth-stencil attachment must declare a *matching-format*
//!   one of its own, verified the hard way (a `depth_stencil: None`
//!   pipeline validation-errors at draw time against this pass's real
//!   `Depth32Float` attachment, it does not silently no-op). The
//!   equivalent-in-effect substitute is [`wgpu::CompareFunction::Always`]
//!   (every fragment passes — nothing to get the sign of) with
//!   `depth_write_enabled: false`. This is what makes a tag behind a wall
//!   read as *dimmed* rather than fully hidden — it always draws, faded.
//!
//! Vanilla's color for each pass (`SubmitNodeCollection.java`/`:117`):
//! normal is opaque white (`-1`), see-through is `-2130706433` =
//! `0x81_FFFFFF` — white at alpha `129/255 ≈ 0.506`. Both are plain
//! `BlendFunction.TRANSLUCENT` (`wgpu::BlendState::ALPHA_BLENDING` here);
//! with the normal pass's alpha at `1.0` the blend is a no-op, so draw order
//! between the two passes does not affect the final pixel where both cover
//! the same texel.
//!
//! # Anchor height and distance cutoff
//!
//! * **Distance cutoff**: `64.0` blocks, squared-distance compared against
//!   camera-to-*feet* (`EntityRenderer.extractNameTags`'s default
//!   `nameTagDistance` argument, `EntityRenderer.java`, tested at
//!   `EntityRenderer.java`).
//! * **Anchor**: `feet.y + base_height * scale + 0.5`. The `+0.5` is
//!   `SubmitNodeCollection.java`'s `nameTagAttachment.y + 0.5`; the
//!   `base_height` term is `EntityAttachment.NAME_TAG`'s fallback point,
//!   `AT_HEIGHT = (width, height) -> (0, height, 0)`
//!   (`EntityAttachment.java`, `:25`) — the entity's own hitbox height,
//!   from the real jar-derived census (`lodestone_data::entity_dimensions`),
//!   not a guess. Some vanilla types override this attachment point (a
//!   sitting cat, a sleeping villager); that per-type override table is not
//!   ported — every entity here uses the `AT_HEIGHT` fallback, which is what
//!   the overwhelming majority of named entities (players, standard mobs)
//!   actually get.
//! * **Sneaking suppression**: `Entity.isDiscrete()` gates the see-through
//!   pass off (`SubmitNodeCollection.java`/`:118`) — resolved once, at
//!   `net::entity_snapshot`'s boundary, as [`crate::entities::NameTag::see_through`].
//!
//! # What is deliberately not built
//!
//! * **The background plate.** Vanilla draws a `TEXT_BACKGROUND`/
//!   `TEXT_BACKGROUND_SEE_THROUGH` quad behind the glyphs, coloured from the
//!   `chatOpacity` game option (`SubmitNodeCollection.java`). Not in the
//!   issue's explicit scope checklist and not required for legibility (the
//!   drop shadow already separates text from background); a genuine gap, not
//!   an oversight.
//! * **Per-frame packed-light modulation.** Vanilla forces near-full
//!   brightness for the normal pass
//!   (`LightCoordsUtil.lightCoordsWithEmission(lightCoords, 2)`,
//!   `SubmitNodeCollection.java`) specifically so a nametag stays legible
//!   in the dark — this renderer draws plain full-bright white unconditionally,
//!   which is a close approximation of that emission override rather than a
//!   divergence from it.
//! * **`EntityAttachment` per-type overrides**, the crosshair-look-at
//!   override to `shouldShowName` (`EntityRenderer.java`), scoreboard
//!   team colouring/prefixes and the `belowName` scoreboard line — all
//!   explicitly out of scope per the issue.
//!
//! # Font: the same jar-sourced glyph data the HUD uses, a new draw path
//!
//! [`crate::hud::vanilla_font::VanillaFont`] cannot be reused directly here:
//! its glyph rasteriser is private and its public draw methods emit into
//! `hud/item_icon.rs`'s 2-D screen-space `ColourStream`, and both files are
//! out of scope for this change (a different agent's files, per the task
//! briefing). This module instead calls the same public, jar-sourced data
//! source directly — [`lodestone_assets::font::RasterFont`], loaded with the
//! same `FontLoader::load_raster(&"minecraft:default".parse()?,
//! &FontOptions::none())` call `VanillaFont::load` makes — and re-derives the
//! ink-run walk `VanillaFont::glyph` uses (same advance metrics, same
//! run-length merge), targeting world-space billboard quads instead of
//! screen-space ones. [`jar_manager`]/[`pack_root`] duplicate
//! `hud/vanilla_font.rs`'s own discovery snippet for the same reason that
//! module duplicates it from `crate::resources` — see that module's doc.

use std::path::{Path, PathBuf};

use glam::Vec3;
use lodestone_assets::font::{FontLoader, FontOptions, GlyphRaster, MISSING_ADVANCE, RasterFont, metrics};
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_model::text::{Text, TextColor, TextSpan};
use lodestone_render::entity::camera_orientation;
use lodestone_render::{Camera, DEPTH_FORMAT};

use crate::entities::EntityDraw;

/// One coloured world-space vertex — the same shape as `gpu/debug_lines.rs`'s
/// `DebugLineVertex`, kept as its own type rather than imported cross-module
/// (each pass owning its vertex type is this crate's established pattern;
/// see `gpu/outline.rs`/`gpu/debug_lines.rs`).
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NameTagVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// Vanilla's per-name-tag world scale
/// (`SubmitNodeCollection.java`: `poseStack.scale(0.025F, -0.025F, 0.025F)`)
/// — one logical text pixel is this many world blocks.
const PX_SCALE: f32 = 0.025;

/// The distance cutoff, in blocks (`EntityRenderer.java`).
const MAX_DISTANCE: f32 = 64.0;

/// The padding above the `NAME_TAG` attachment point
/// (`SubmitNodeCollection.java`).
const ATTACHMENT_PADDING: f32 = 0.5;

/// Fallback base hitbox height, in blocks, for a type path the jar-derived
/// census cannot resolve (shouldn't happen for a real registered type, but a
/// malformed/future type id must degrade to *something* rather than crash).
/// The player's own height — a reasonable middle ground.
const FALLBACK_HEIGHT: f32 = 1.8;

/// Opaque white — the normal pass's colour (`-1` in `SubmitNodeCollection.java`).
/// Only its alpha (`1.0`) is read now: the RGB half of a `StyledRect` is
/// already resolved (white when a span's own colour is unspecified, real
/// per-span colour otherwise) by [`layout_styled_ink_runs`] — see
/// [`push_entity_quads`]'s doc for the per-pass alpha/colour split this and
/// [`SEE_THROUGH_COLOR`] now supply.
const NORMAL_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// White at `129/255`, vanilla's `-2130706433` (`0x81_FFFFFF`) — the
/// see-through pass's colour (`SubmitNodeCollection.java`). Same "alpha
/// only" reading as [`NORMAL_COLOR`].
const SEE_THROUGH_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 129.0 / 255.0];

/// Fixed vertex capacity per pass (six vertices per glyph-row ink run). Same
/// fixed-buffer idiom as `gpu/debug_lines.rs`'s `MAX_DEBUG_LINE_SEGMENTS` —
/// see that constant's doc for why a fixed cap is what lets `prepare` take
/// `&self`. Comfortably above what a screen full of named mobs needs: an
/// 8-row-tall glyph contributes at most 8 runs, so this covers roughly 1,700
/// glyphs' worth of ink in one frame.
const MAX_NAME_TAG_VERTICES: usize = 60_000;

/// A local "logical pixel" rect of ink, plus
/// this run's own resolved RGBA colour (opaque, `alpha` always `1.0` — see
/// [`layout_styled_ink_runs`]'s doc for why alpha is deliberately not baked
/// in here). A world-space equivalent of `hud/vanilla_font.rs::ResolvedGlyph`
/// flattened straight to draw-ready geometry, the way this layout already
/// is for the unstyled path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StyledRect {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
    pub(super) color: [f32; 4],
}

/// One string's finished *styled* ink-run layout — the spanned sibling of
/// the cache below.
pub(super) type StyledInkLayout = std::sync::Arc<(Vec<StyledRect>, f32)>;

/// Persisted [`layout_styled_ink_runs`] results, keyed by the resolved span
/// list rather than a bare string — [`TextSpan`] already derives `Hash`/`Eq`
/// for exactly this (see its own doc: "what lets a `Vec<TextSpan>` key a wrap
/// cache the way a plain `String` already keys `hud::ChatWrapCache`"). Same
/// shape and reasoning as the nametag layout it serves; kept as a
/// separate type rather than a generic cache because the two key types don't
/// share a cheap `Borrow` conversion worth building for two call sites.
#[derive(Debug, Default)]
pub(super) struct StyledInkLayoutCache {
    inner: std::sync::Mutex<std::collections::HashMap<Vec<TextSpan>, StyledInkLayout>>,
}

impl StyledInkLayoutCache {
    /// Cleared wholesale past this many distinct span lists — same policy as
    /// [`Self::MAX_ENTRIES`].
    const MAX_ENTRIES: usize = 512;

    /// This span list's styled ink-run layout, walking the texels only on a
    /// miss.
    pub(super) fn layout(&self, raster: &RasterFont, spans: &[TextSpan]) -> StyledInkLayout {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(hit) = map.get(spans) {
            return std::sync::Arc::clone(hit);
        }
        if map.len() >= Self::MAX_ENTRIES {
            map.clear();
        }
        let layout: StyledInkLayout = std::sync::Arc::new(layout_styled_ink_runs(raster, spans));
        map.insert(spans.to_vec(), std::sync::Arc::clone(&layout));
        layout
    }
}

/// [`TextColor::rgb`] as an sRGB `0..1` triple, falling back to `base` for an
/// unspecified (`None`) colour — [`TextStyle::color`](lodestone_model::text::TextStyle::color)'s
/// own "inherit reaches the root, the *surface* decides" contract (see
/// `hud/vanilla_font.rs::resolve_spans`'s identical fallback, for the
/// screen-space path). Duplicated rather than calling
/// `hud::vanilla_font::text_color_rgb` directly — this module already
/// duplicates that file's font-discovery snippet for the same "small,
/// self-contained, not worth a cross-module dependency" reasoning; see the
/// module doc.
fn resolved_rgb(color: Option<TextColor>, base: [f32; 3]) -> [f32; 3] {
    let Some(color) = color else { return base };
    let hex = color.rgb();
    [
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ]
}

/// The world-space ink walk: takes a fully-inherited span
/// list (`Text::to_spans`'s own output — [`crate::entities::NameTag::text`]/
/// `crate::display_entities::DisplayDraw::text` are real
/// [`lodestone_model::text::Text`] values now, so both callers here call
/// `to_spans` on them directly with no legacy-string bridge in between)
/// instead of a bare string, so a coloured/bold/italic/underlined/struck-through
/// run reaches world-space geometry with its own style intact.
///
/// This replaced an earlier unstyled walk that painted every rect in one
/// uniform colour, so a dropped colour read as plain text. Every
/// [`StyledRect`] this one emits carries its **own** resolved
/// colour, so a caller no longer supplies one flat colour for the whole
/// string — it supplies one only as the *fallback* for a span whose colour is
/// unspecified (`base_rgb`, always opaque white for both of today's callers:
/// vanilla hardcodes white as the base tint for both nametags
/// (`SubmitNodeCollection.java`) and `text_display` (`TextDisplayRenderer.submitInner`'s
/// `textOpacity << 24 | 16777215`), and only a real per-span [`TextColor`]
/// overrides it — see `Font.java::getTextColor`, which is exactly this
/// function's `resolved_rgb`).
///
/// Alpha is deliberately **not** threaded through here (every [`StyledRect`]
/// is opaque, alpha `1.0`): nametags draw the *same* cached layout at three
/// different alphas (shadow at the normal pass's alpha, normal pass at
/// `1.0`, the see-through pass at `129/255`), and `text_display` draws it at
/// a per-entity `textOpacity`-derived alpha — baking any one of those into
/// the cached geometry would make the cache wrong for every other consumer
/// of the same span list. Callers multiply their own alpha onto
/// [`StyledRect::color`]'s existing `1.0` when building vertices.
///
/// Bold widens the **advance** (`GlyphInfo.getAdvance(bold)`, `Font.java`),
/// which is why a styled line's width cannot be measured by a plain
/// per-codepoint advance walk: a bold run measures
/// wider than the same codepoints unstyled, and a caller that centres a
/// multi-line block against an *unstyled* per-line width mis-centres exactly
/// the line carrying the wider bold run — see `gpu/display_text.rs`'s module
/// doc for the alignment defect this was written to close.
///
/// Underline/strikethrough are emitted **per glyph**, matching
/// `Font.java::accept`'s own unconditional per-glyph effect bar (including
/// for whitespace) rather than one bar merged across a run — simpler to keep
/// obviously correct against the source, at the cost of more (touching,
/// visually identical) rects. `§k` obfuscation is not implemented: it needs
/// per-frame resampling state neither caller here keeps, and is disclosed as
/// a real gap rather than half-built.
pub(super) fn layout_styled_ink_runs(raster: &RasterFont, spans: &[TextSpan]) -> (Vec<StyledRect>, f32) {
    const BASE_RGB: [f32; 3] = [1.0, 1.0, 1.0];

    let mut cursor = 0.0f32;
    let mut rects = Vec::new();
    let mut position = 0usize;
    for span in spans {
        let rgb = resolved_rgb(span.style.color, BASE_RGB);
        let color = [rgb[0], rgb[1], rgb[2], 1.0];
        let bold = span.style.bold.unwrap_or(false);
        let italic = span.style.italic.unwrap_or(false);
        let underlined = span.style.underlined.unwrap_or(false);
        let strikethrough = span.style.strikethrough.unwrap_or(false);

        for ch in span.text.chars() {
            let cp = ch as u32;
            let x0 = cursor;
            let glyph_raster = raster.raster(cp);
            let base_advance = glyph_raster
                .as_ref()
                .map_or_else(|| raster.advance(cp).unwrap_or(MISSING_ADVANCE), GlyphRaster::advance);
            let bold_extra = raster.font().bold_offset(cp);
            let advance = if bold { base_advance + bold_extra } else { base_advance };

            if let Some(r) = &glyph_raster {
                let texel = r.texel_size();
                let top = r.top();
                let left = r.left();
                for ty in 0..r.cell_height() {
                    let mut tx = 0;
                    while tx < r.cell_width() {
                        if !r.is_ink(tx, ty) {
                            tx += 1;
                            continue;
                        }
                        let run_start = tx;
                        while tx < r.cell_width() && r.is_ink(tx, ty) {
                            tx += 1;
                        }
                        let shear = if italic {
                            let v = top + (ty as f32 + 0.5) * texel;
                            font_shear(v)
                        } else {
                            0.0
                        };
                        let rx = x0 + left + shear + run_start as f32 * texel;
                        let ry = top + ty as f32 * texel;
                        let rw = (tx - run_start) as f32 * texel;
                        rects.push(StyledRect { x: rx, y: ry, w: rw, h: texel, color });
                        if bold {
                            rects.push(StyledRect { x: rx + bold_extra, y: ry, w: rw, h: texel, color });
                        }
                    }
                }
            }

            if underlined || strikethrough {
                // `Font.java`: `effectX0 = position == 0 ? x - 1.0F : x` —
                // `position` counts glyphs across the *whole* line, not per
                // span, so this must survive the span boundary above.
                let effect_x0 = if position == 0 {
                    x0 - metrics::EFFECT_LEAD_IN
                } else {
                    x0
                };
                let effect_x1 = x0 + advance;
                let thickness = metrics::EFFECT_THICKNESS;
                if strikethrough {
                    let bottom = metrics::STRIKETHROUGH_Y;
                    rects.push(StyledRect {
                        x: effect_x0,
                        y: bottom - thickness,
                        w: effect_x1 - effect_x0,
                        h: thickness,
                        color,
                    });
                }
                if underlined {
                    let bottom = metrics::UNDERLINE_Y;
                    rects.push(StyledRect {
                        x: effect_x0,
                        y: bottom - thickness,
                        w: effect_x1 - effect_x0,
                        h: thickness,
                        color,
                    });
                }
            }

            cursor += advance;
            position += 1;
        }
    }
    (rects, cursor)
}

/// `BakedSheetGlyph.shearTop`/`shearBottom` (`BakedSheetGlyph.java`, both
/// `1.0F - 0.25F * v`) evaluated at one texel row's own local `v` — the same
/// per-row shear `hud/vanilla_font.rs::draw_ink` already applies, transcribed
/// here rather than called cross-module for the same "small, self-contained"
/// reasoning as [`resolved_rgb`].
fn font_shear(v: f32) -> f32 {
    metrics::ITALIC_SHEAR - metrics::ITALIC_SHEAR_SLOPE * v
}

/// This type path's base hitbox height (`lodestone_data::entity_dimensions`),
/// scaled by nothing yet — the caller multiplies by [`EntityDraw::scale`].
/// Falls back to [`FALLBACK_HEIGHT`] for a type path the census cannot
/// resolve (no `minecraft:<path>` id, or a `0`-height marker type).
///
/// Resolves through [`lodestone_data::entity_type::EntityType::from_name`] (a
/// binary search over the generated registry, issue #523) rather than
/// `entity_type_id_parts`'s linear `strip_prefix` scan — this runs once per
/// named entity per frame, not once per spawn, so the O(158) scan was real
/// per-frame cost.
#[must_use]
fn entity_base_height(type_path: &str) -> f32 {
    lodestone_data::entity_type::EntityType::from_name(type_path)
        .map(lodestone_data::entity_dimensions::base_dimensions_for)
        .map(|dims| dims.height)
        .filter(|h| *h > 0.0)
        .unwrap_or(FALLBACK_HEIGHT)
}

/// Turns one local rect into two triangles (six vertices, no index buffer) in
/// world space, billboarded with the frame's shared `right`/`up` basis —
/// every nametag this frame shares the same basis, matching vanilla's single
/// `camera.orientation` applied identically to each
/// (`SubmitNodeCollection.java`: `poseStack.mulPose(camera.orientation)`,
/// *before* any per-entity translation).
///
/// No culling is configured on either pipeline (`cull_mode: None`, `wgpu`'s
/// default), so winding order here is deliberately not load-bearing — unlike
/// `docs/gpu-module-layout.md`'s GUI-winding invariant, a billboard that only
/// the camera it faces ever sees needs no back face at all.
fn quad_vertices(
    rect: StyledRect,
    half_width: f32,
    anchor: Vec3,
    right: Vec3,
    up: Vec3,
    color: [f32; 4],
) -> [NameTagVertex; 6] {
    let to_world = |lx: f32, ly: f32| -> [f32; 3] {
        let p = anchor + right * ((lx - half_width) * PX_SCALE) + up * (-ly * PX_SCALE);
        [p.x, p.y, p.z]
    };
    let tl = to_world(rect.x, rect.y);
    let tr = to_world(rect.x + rect.w, rect.y);
    let bl = to_world(rect.x, rect.y + rect.h);
    let br = to_world(rect.x + rect.w, rect.y + rect.h);
    [
        NameTagVertex {
            position: tl,
            color,
        },
        NameTagVertex {
            position: bl,
            color,
        },
        NameTagVertex {
            position: tr,
            color,
        },
        NameTagVertex {
            position: tr,
            color,
        },
        NameTagVertex {
            position: bl,
            color,
        },
        NameTagVertex {
            position: br,
            color,
        },
    ]
}

/// Lowers one entity's [`EntityDraw::name_tag`] into world-space vertices,
/// appended onto `normal_out` (and, when [`crate::entities::NameTag::see_through`]
/// is set, `see_through_out`). A no-op for an entity with no tag, an empty
/// tag, or one further than [`MAX_DISTANCE`] from the camera.
fn push_entity_quads(
    raster: &RasterFont,
    ink: &StyledInkLayoutCache,
    draw: &EntityDraw,
    camera_position: Vec3,
    right: Vec3,
    up: Vec3,
    normal_out: &mut Vec<NameTagVertex>,
    see_through_out: &mut Vec<NameTagVertex>,
) {
    let Some(tag) = &draw.name_tag else {
        return;
    };
    if camera_position.distance_squared(draw.feet) > MAX_DISTANCE * MAX_DISTANCE {
        return;
    }

    let height = entity_base_height(&draw.type_path) * draw.scale;
    let anchor = draw.feet + Vec3::new(0.0, height + ATTACHMENT_PADDING, 0.0);

    // `NameTag::text` is a real `Text` (`crate::entities`' tab-list/
    // custom-name resolution now carries the full component tree, not a
    // flattened plain string), so `to_spans` reads colour — hex included,
    // which no `to_legacy_string`/`from_legacy` round trip could carry, since
    // legacy `§` codes have no hex form — bold, italic, underline and
    // strikethrough straight off it with no bridge in between.
    let spans = tag.text.to_spans();

    // Cached: the walk depends only on `(spans, font)`, and this is called
    // once per visible named entity per frame.
    let layout = ink.layout(raster, &spans);
    let (rects, total_width) = (&layout.0, layout.1);
    if rects.is_empty() {
        // Covers both "no name tag text at all" and "every span was empty"
        // — `Text::to_spans` never emits an empty-text span (`collect_spans`
        // skips `own.is_empty()`), so an all-empty tree yields zero spans and
        // this is reached with no texel walk performed.
        return;
    }
    let half_width = total_width / 2.0;

    // The shadow copy first (whole string), then the text — same order
    // `VanillaFont::draw` uses, for the same reason (a later glyph's ink
    // must sit on top of an earlier glyph's shadow, not the other way
    // round). The shadow's own colour follows the glyph's resolved colour
    // scaled to a quarter (`Font.java::getShadowColor`'s no-explicit-colour
    // branch, `ARGB.scaleRGB(textColor, 0.25F)`), not a flat grey — so a
    // coloured name's shadow is a dim version of *that* colour.
    let shadow_offset = metrics::SHADOW_OFFSET;
    for rect in rects {
        let shadow_rect = StyledRect {
            x: rect.x + shadow_offset,
            y: rect.y + shadow_offset,
            ..*rect
        };
        let color = [
            rect.color[0] * metrics::SHADOW_BRIGHTNESS,
            rect.color[1] * metrics::SHADOW_BRIGHTNESS,
            rect.color[2] * metrics::SHADOW_BRIGHTNESS,
            NORMAL_COLOR[3],
        ];
        normal_out.extend(quad_vertices(shadow_rect, half_width, anchor, right, up, color));
    }
    for rect in rects {
        // The normal pass's own alpha is `1.0` and every `StyledRect` is
        // already opaque, so the pass contributes nothing beyond the
        // resolved colour itself — unlike the see-through pass below, which
        // overrides alpha but keeps the same resolved colour.
        normal_out.extend(quad_vertices(*rect, half_width, anchor, right, up, rect.color));
    }
    if tag.see_through {
        for rect in rects {
            let color = [rect.color[0], rect.color[1], rect.color[2], SEE_THROUGH_COLOR[3]];
            see_through_out.extend(quad_vertices(*rect, half_width, anchor, right, up, color));
        }
    }
}

/// Draws billboarded nametag text above every [`EntityDraw`] carrying one —
/// see the module doc for the two depth passes' exact settings.
#[derive(Debug)]
pub(super) struct NameTagRenderer {
    normal_pipeline: wgpu::RenderPipeline,
    see_through_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    normal_vertices: wgpu::Buffer,
    see_through_vertices: wgpu::Buffer,
    /// `None` off a jar-less run (headless gates, the demo world) — the same
    /// fail-open contract [`crate::hud::vanilla_font::VanillaFont::shared`]
    /// has, and for the same reason: nothing here is a hard requirement, and
    /// every caller below already treats "no font" as "draw nothing" rather
    /// than panicking.
    font: Option<RasterFont>,
    /// Ink-run layouts, persisted across frames.
    ink: StyledInkLayoutCache,
}

impl NameTagRenderer {
    pub(super) fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-nametag-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/nametag.wgsl").into()),
        });

        // One `view_proj` uniform, nothing else — same shape as
        // `gpu/debug_lines.rs`'s bind-group layout, so this pass has no
        // bearing on the model shader's 4-bind-group floor
        // (`docs/gpu-module-layout.md`).
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-nametag-bgl"),
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
            label: Some("lodestone-nametag-uniform"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-nametag-bg"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        let buffer_size =
            (MAX_NAME_TAG_VERTICES * std::mem::size_of::<NameTagVertex>()) as u64;
        let normal_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-nametag-normal-vertices"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let see_through_vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-nametag-see-through-vertices"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-nametag-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let vertex_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<NameTagVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        })];

        let build = |label: &str, depth_stencil: Option<wgpu::DepthStencilState>| {
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
                    ..Default::default()
                },
                depth_stencil,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        // Normal pass: depth-tested and depth-written, `LessEqual` — the
        // sign-flipped port of vanilla's reversed-Z `GREATER_THAN_OR_EQUAL`
        // (`DepthStencilState.DEFAULT`, see the module doc).
        let normal_pipeline = build(
            "lodestone-nametag-normal-pipeline",
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        );
        // See-through pass: vanilla's `TEXT_SEE_THROUGH` pipeline has
        // `Optional.empty()` for its whole depth-stencil state — no depth
        // attachment at all. `wgpu` has no equivalent for "this pipeline
        // uses no depth attachment" *while sharing a render pass that has
        // one*: every pipeline drawn inside a pass with a depth-stencil
        // attachment must declare a matching format, verified the hard way —
        // `depth_stencil: None` here validation-errors at draw time
        // ("Incompatible depth-stencil attachment format: … Some(Depth32Float)
        // but the RenderPipeline … uses an attachment with format None"),
        // it does not silently no-op. `CompareFunction::Always` (every
        // fragment passes, matching the attachment's format so the pass is
        // valid) plus `depth_write_enabled: false` is the equivalent-in-effect
        // substitute: no comparison operator to get the sign of, and no write
        // — precisely "no depth interaction" within the constraint that the
        // pipeline must still name the format.
        let see_through_pipeline = build(
            "lodestone-nametag-see-through-pipeline",
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        );

        Self {
            normal_pipeline,
            see_through_pipeline,
            bind_group,
            uniform,
            normal_vertices,
            see_through_vertices,
            font: load_font(),
            ink: StyledInkLayoutCache::default(),
        }
    }

    /// Uploads this frame's view-projection and nametag vertices. Must run
    /// before the render pass opens (buffers cannot be written mid-pass).
    /// Returns `(normal_vertex_count, see_through_vertex_count)`, capped at
    /// [`MAX_NAME_TAG_VERTICES`] each — pass to [`draw`](Self::draw).
    pub(super) fn prepare(
        &self,
        queue: &wgpu::Queue,
        view_proj: &[[f32; 4]; 4],
        camera: &Camera,
        entities: &[EntityDraw],
    ) -> (u32, u32) {
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(view_proj));
        let Some(raster) = &self.font else {
            return (0, 0);
        };

        // The frame's shared camera-facing basis — every nametag this frame
        // billboards off the same rotation, matching vanilla (see
        // `quad_vertices`'s doc).
        let orientation = camera_orientation(camera.view_matrix());
        let right = orientation.x_axis.truncate();
        let up = orientation.y_axis.truncate();

        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        for draw in entities {
            push_entity_quads(
                raster,
                &self.ink,
                draw,
                camera.position,
                right,
                up,
                &mut normal,
                &mut see_through,
            );
        }
        let normal_len = normal.len().min(MAX_NAME_TAG_VERTICES);
        let see_through_len = see_through.len().min(MAX_NAME_TAG_VERTICES);
        if normal_len > 0 {
            queue.write_buffer(
                &self.normal_vertices,
                0,
                bytemuck::cast_slice(&normal[..normal_len]),
            );
        }
        if see_through_len > 0 {
            queue.write_buffer(
                &self.see_through_vertices,
                0,
                bytemuck::cast_slice(&see_through[..see_through_len]),
            );
        }
        (normal_len as u32, see_through_len as u32)
    }

    /// Records both passes' draws (whichever have vertices). No-op with the
    /// no-jar `font: None` state, since [`prepare`](Self::prepare) always
    /// returns `(0, 0)` there.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, counts: (u32, u32)) {
        let (normal_count, see_through_count) = counts;
        if normal_count > 0 {
            pass.set_pipeline(&self.normal_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.normal_vertices.slice(..));
            pass.draw(0..normal_count, 0..1);
        }
        if see_through_count > 0 {
            pass.set_pipeline(&self.see_through_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.see_through_vertices.slice(..));
            pass.draw(0..see_through_count, 0..1);
        }
    }
}

/// Loads the vanilla `minecraft:default` font's raster data for world-space
/// drawing. `None` off a jar-less run — see [`NameTagRenderer::font`].
///
/// `pub(super)`: `gpu/sign_text.rs` calls this directly rather than
/// duplicating `jar_manager`/`pack_root` a third time — this module's own
/// doc already explains why *those* are duplicated from `hud/vanilla_font.rs`
/// (a different agent's off-limits file at the time), but nothing stops a
/// sibling `gpu` submodule reusing what is already here.
pub(super) fn load_font() -> Option<RasterFont> {
    let manager = jar_manager()?;
    let id: lodestone_assets::ResourceLocation = "minecraft:default".parse().ok()?;
    match FontLoader::new(&manager).load_raster(&id, &FontOptions::none()) {
        Ok(raster) => Some(raster),
        Err(e) => {
            tracing::warn!(target: "assets", "load nametag font: {e}");
            None
        }
    }
}

/// Open `client.jar` from a discovered vanilla pack root as a
/// [`ResourceManager`]. A deliberate duplicate of
/// `hud/vanilla_font.rs::jar_manager` — see this module's doc for why this
/// file cannot call that one directly, and `hud/vanilla_font.rs`'s own doc
/// for why *it* duplicates `crate::resources` rather than calling the
/// `#[cfg(test)]`-gated original.
fn jar_manager() -> Option<ResourceManager> {
    let jar = pack_root()?.join("client.jar");
    let bytes = std::fs::read(&jar)
        .map_err(|e| tracing::warn!(target: "assets", "read {}: {e}", jar.display()))
        .ok()?;
    let zip = ZipSource::from_bytes(bytes)
        .map_err(|e| tracing::warn!(target: "assets", "open {}: {e}", jar.display()))
        .ok()?;
    Some(ResourceManager::new(vec![
        Box::new(zip) as Box<dyn ResourceSource>,
    ]))
}

fn pack_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LODESTONE_ASSETS") {
        let p = PathBuf::from(dir);
        return is_pack_root(&p).then_some(p);
    }
    let cwd = std::env::current_dir().ok()?;
    for base in cwd.ancestors() {
        let cache = base.join(".cache/mc");
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(&cache) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| is_pack_root(p))
                .collect(),
            Err(_) => continue,
        };
        entries.sort();
        if let Some(root) = entries.pop() {
            return Some(root);
        }
    }
    None
}

fn is_pack_root(dir: &Path) -> bool {
    dir.join("client.jar").is_file() && dir.join("generated/reports/blocks.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic font-free environment must draw nothing rather than
    /// panic — same "no jar, no crash" contract every other jar-optional
    /// path in this crate has. Exercises `push_entity_quads` directly so the
    /// gate does not depend on whether this machine happens to have a jar.
    #[test]
    fn an_entity_beyond_max_distance_contributes_no_vertices() {
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: std::sync::Arc::from("pig"),
            item: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            feet: Vec3::new(0.0, 0.0, MAX_DISTANCE + 1.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
            name_tag: Some(crate::entities::NameTag {
                text: Text::literal("Babe"),
                see_through: true,
            }),
            item_use: None,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
            // A pig, not a player.
            swim_amount: 0.0,
            death_time: 0.0,
            // Not on fire: these literals exist to position a nametag, not to
            // draw the entity, so no flame billboard is wanted.
            on_fire: false,
            // Not invisible and not an armour stand — same reasoning as
            // `on_fire`, these literals exist to position a nametag.
            invisible: false,
            armor_stand: None,
            // Not a player, so no skin can apply.
            player_skin: None,
            variant_sheet: None,
            // Not an experience orb: `None` keeps this subject out of the orb
            // billboard pass entirely.
            experience_orb_value: None,
            cape_sway: (0.0, 0.0, 0.0),
        };
        // A raster is required to reach the distance check at all in
        // `prepare`, but `push_entity_quads` itself only needs one to lay
        // out ink runs *after* the distance check passes — so this control
        // needs no jar and no `RasterFont` fixture.
        let Some(raster) = load_font() else {
            // No jar on this machine: the distance gate is exercised by the
            // live pixel gate instead (`tests/nametag_pixels.rs`), which
            // requires one. Nothing to assert here without a raster.
            return;
        };
        push_entity_quads(
            &raster,
            &StyledInkLayoutCache::default(),
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal,
            &mut see_through,
        );
        assert!(
            normal.is_empty() && see_through.is_empty(),
            "an entity past the 64-block cutoff must contribute nothing, got {} + {} vertices",
            normal.len(),
            see_through.len()
        );
    }

    /// The positive control for the same gate: move the same entity just
    /// inside the cutoff and it must contribute real ink.
    #[test]
    fn an_entity_within_max_distance_with_a_name_contributes_vertices() {
        let Some(raster) = load_font() else {
            return;
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: std::sync::Arc::from("pig"),
            item: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            feet: Vec3::new(0.0, 0.0, MAX_DISTANCE - 1.0),
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
            name_tag: Some(crate::entities::NameTag {
                text: Text::literal("Babe"),
                see_through: true,
            }),
            item_use: None,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
            // A pig, not a player.
            swim_amount: 0.0,
            death_time: 0.0,
            // Not on fire: these literals exist to position a nametag, not to
            // draw the entity, so no flame billboard is wanted.
            on_fire: false,
            // Not invisible and not an armour stand — same reasoning as
            // `on_fire`, these literals exist to position a nametag.
            invisible: false,
            armor_stand: None,
            // Not a player, so no skin can apply.
            player_skin: None,
            variant_sheet: None,
            // Not an experience orb: `None` keeps this subject out of the orb
            // billboard pass entirely.
            experience_orb_value: None,
            cape_sway: (0.0, 0.0, 0.0),
        };
        push_entity_quads(
            &raster,
            &StyledInkLayoutCache::default(),
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal,
            &mut see_through,
        );
        assert!(
            !normal.is_empty(),
            "an in-range named entity must contribute normal-pass ink"
        );
        assert!(
            !see_through.is_empty(),
            "`see_through: true` must contribute see-through-pass ink too"
        );

        // Negative control on `see_through`: the same entity, sneaking,
        // must contribute normal ink but none to the see-through pass.
        let mut normal2 = Vec::new();
        let mut see_through2 = Vec::new();
        let sneaking = EntityDraw {
            name_tag: Some(crate::entities::NameTag {
                text: Text::literal("Babe"),
                see_through: false,
            }),
            ..draw
        };
        push_entity_quads(
            &raster,
            &StyledInkLayoutCache::default(),
            &sneaking,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal2,
            &mut see_through2,
        );
        assert!(
            !normal2.is_empty(),
            "sneaking must not suppress the normal pass"
        );
        assert!(
            see_through2.is_empty(),
            "sneaking (`see_through: false`) must suppress the see-through pass"
        );
    }

    /// A blank custom name (empty string) must draw nothing — same rule as
    /// "no name tag at all", not a zero-width visible tag.
    #[test]
    fn an_empty_name_contributes_no_vertices() {
        let Some(raster) = load_font() else {
            return;
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: std::sync::Arc::from("pig"),
            item: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            feet: Vec3::ZERO,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
            name_tag: Some(crate::entities::NameTag {
                text: Text::default(),
                see_through: true,
            }),
            item_use: None,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
            // A pig, not a player.
            swim_amount: 0.0,
            death_time: 0.0,
            // Not on fire: these literals exist to position a nametag, not to
            // draw the entity, so no flame billboard is wanted.
            on_fire: false,
            // Not invisible and not an armour stand — same reasoning as
            // `on_fire`, these literals exist to position a nametag.
            invisible: false,
            armor_stand: None,
            // Not a player, so no skin can apply.
            player_skin: None,
            variant_sheet: None,
            // Not an experience orb: `None` keeps this subject out of the orb
            // billboard pass entirely.
            experience_orb_value: None,
            cape_sway: (0.0, 0.0, 0.0),
        };
        push_entity_quads(
            &raster,
            &StyledInkLayoutCache::default(),
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            &mut normal,
            &mut see_through,
        );
        assert!(normal.is_empty() && see_through.is_empty());
    }

    #[test]
    fn entity_base_height_falls_back_for_an_unknown_type_path() {
        assert_eq!(entity_base_height("not_a_real_entity_type"), FALLBACK_HEIGHT);
        // A real type resolves to its real (non-fallback) census height.
        assert!((entity_base_height("player") - 1.8).abs() < 1e-6);
    }

    /// Behavioural gate for issue #523: `entity_base_height` now resolves
    /// through `EntityType::from_name`'s binary search rather than
    /// `entity_type_id_parts`'s linear `strip_prefix` scan. The expected
    /// heights come from `crates/lodestone-data/tests/support/
    /// entity_dimensions_jvm.txt` (a real 26.2 server's own
    /// `EntityType.getHeight()` dump), an outside source, not from this
    /// table round-tripping against itself.
    ///
    /// `chicken` and `enderman` are chosen because they are far apart both
    /// in registry id (26 vs 41) and in height (0.7 vs 2.9): a wrong sort
    /// order in the new binary search would resolve one of these bare paths
    /// to a *neighbouring* real entity type rather than failing outright —
    /// a different, plausible height, not an obvious `None`/panic — and a
    /// pair this far apart cannot coincidentally agree the way `bogged` and
    /// `skeleton` do (both `1.99`, see `lodestone-render`'s
    /// `canonical_model_name` alias).
    #[test]
    fn entity_base_height_matches_the_jvm_dump_for_a_discriminating_pair() {
        assert!((entity_base_height("chicken") - 0.7).abs() < 1e-3);
        assert!((entity_base_height("enderman") - 2.9).abs() < 1e-3);
    }

    /// A hermetic bitmap-font fixture with an explicit declared `height`
    /// independent of the source image's row height -- the only way to get
    /// `pixel_scale != 1.0`. No fixture elsewhere in this file exercises
    /// that (every one goes through the real jar via [`load_font`], whose
    /// own three sheets are all `pixel_scale == 1.0`), which is exactly the
    /// coincidence `hud::vanilla_font`'s
    /// `bitmap_draw_extent_respects_pixel_scale_not_just_advance` test
    /// documents: at `pixel_scale == 1.0`, "multiply the cell's texel walk
    /// by `pixel_scale`" and "don't" are indistinguishable.
    fn scaled_raster(chars: &str, cell: u32, declared_height: u32, rgba: &[u8]) -> RasterFont {
        let width = cell * chars.chars().count() as u32;
        let mut png = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png, width, cell);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(rgba)
                .unwrap();
        }
        let mut source = lodestone_assets::MemorySource::new("nametag-scaled-fixture");
        source.insert("assets/minecraft/textures/font/t.png", png);
        source.insert(
            "assets/minecraft/font/default.json",
            format!(
                r#"{{"providers":[{{"type":"bitmap","file":"minecraft:font/t.png","ascent":1,"height":{declared_height},"chars":["{chars}"]}}]}}"#
            )
            .into_bytes(),
        );
        let manager = ResourceManager::new(vec![Box::new(source)]);
        let id: lodestone_assets::ResourceLocation =
            "minecraft:default".parse().expect("valid fixture id");
        FontLoader::new(&manager)
            .load_raster(&id, &FontOptions::none())
            .expect("scaled bitmap fixture font loads")
    }

    /// The nametag/sign-text ink walk ([`layout_styled_ink_runs`]) is a
    /// *second*, independent implementation of the same "cell texels scaled
    /// by `pixel_scale`" logic `hud::vanilla_font::draw_ink` has -- CLAUDE.md's
    /// own point about a hazard fixed once and not shared: nothing
    /// mechanical connects the two, so this needs its own control rather
    /// than inheriting the HUD path's. Same discriminating input: two
    /// fully-opaque glyphs at a non-integer `pixel_scale` (0.625); if the
    /// cell's texel walk here ever used the raw physical cell width instead
    /// of `cell_width * pixel_scale`, the rects for 'B' would start well
    /// inside 'A''s own (wrongly large) rects.
    #[test]
    fn layout_styled_ink_runs_respects_pixel_scale_not_just_advance() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);

        let spans = Text::from_legacy("AB").to_spans();
        let (rects, total_advance) = layout_styled_ink_runs(&raster, &spans);
        assert!(!rects.is_empty(), "the opaque cells must produce ink rects");
        // Same arithmetic as the HUD-path control: advance(A) = (int)(0.5 +
        // 32*0.625) + 1 = 21; each glyph's own drawn extent is 32*0.625=20.
        assert_eq!(
            raster.advance('A' as u32),
            Some(21.0),
            "advance itself must already reflect pixel_scale"
        );
        let max_x = rects
            .iter()
            .map(|r| r.x + r.w)
            .fold(f32::MIN, f32::max);
        let min_x = rects.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        assert!((min_x - 0.0).abs() < 0.01, "got min_x={min_x}, want 0.0");
        // Correct total extent: A's [0,20] then B's pen at 21, drawing its
        // own [0,20] local -> screen [21,41] -> overall max 41. The wrong
        // hypothesis (raw 32px cell instead of the scaled 20px one) would
        // give 21+32=53, and A's own wrongly-large [0,32] would overlap B's
        // [21,53] from x=21 to x=32.
        assert!(
            (max_x - 41.0).abs() < 0.01,
            "got max_x={max_x}, want 41.0 (32px raw cell would give 53.0 -- \
             layout_styled_ink_runs must scale the cell's texel walk by pixel_scale, \
             not just the advance)"
        );
        assert!((total_advance - 42.0).abs() < 0.01);
    }

    /// **The colour control**, at [`layout_styled_ink_runs`] directly, using
    /// the same synthetic (jar-free) fixture as
    /// `layout_styled_ink_runs_respects_pixel_scale_not_just_advance` so this runs
    /// deterministically in CI regardless of whether a real jar is present.
    ///
    /// A `§c`-coloured span must produce rects whose colour is red; the same
    /// codepoint with no colour code must produce rects at the plain white
    /// fallback, not red — the discriminating pair. Before this function
    /// existed, the unstyled ink walk it replaced painted every rect in one
    /// uniform colour, so a dropped colour read as plain text and no
    /// assertion could distinguish coloured from uncoloured input at all.
    #[test]
    fn layout_styled_ink_runs_resolves_a_spans_own_colour() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);

        let red_spans = Text::from_legacy("\u{a7}cA").to_spans();
        let (red_rects, _) = layout_styled_ink_runs(&raster, &red_spans);
        assert!(!red_rects.is_empty(), "an opaque glyph must still produce ink rects when coloured");
        let hex = TextColor::Red.rgb();
        let want_red = [
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
            1.0,
        ];
        assert!(
            red_rects.iter().all(|r| r.color == want_red),
            "every rect from a §c span must resolve to red: {:?}",
            red_rects.iter().map(|r| r.color).collect::<Vec<_>>()
        );

        let plain_spans = Text::from_legacy("A").to_spans();
        let (plain_rects, _) = layout_styled_ink_runs(&raster, &plain_spans);
        assert!(
            plain_rects.iter().all(|r| r.color == [1.0, 1.0, 1.0, 1.0]),
            "uncoloured text must fall back to the base white, not red — the \
             fixture must be able to tell coloured from uncoloured: {:?}",
            plain_rects.iter().map(|r| r.color).collect::<Vec<_>>()
        );
    }

    /// **The bold control**, same fixture: a bold run must draw its ink
    /// *twice* (`BakedSheetGlyph.renderChar`'s second, offset pass — see
    /// [`layout_styled_ink_runs`]'s doc) and must measure a wider advance
    /// (`GlyphInfo.getAdvance(bold)`) than the identical codepoint unstyled —
    /// the exact width difference `gpu/display_text.rs`'s alignment fix
    /// depends on.
    #[test]
    fn layout_styled_ink_runs_doubles_ink_and_widens_advance_for_bold() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);

        let plain_spans = Text::from_legacy("A").to_spans();
        let (plain_rects, plain_width) = layout_styled_ink_runs(&raster, &plain_spans);

        let bold_spans = Text::from_legacy("\u{a7}lA").to_spans();
        let (bold_rects, bold_width) = layout_styled_ink_runs(&raster, &bold_spans);

        assert_eq!(
            bold_rects.len(),
            plain_rects.len() * 2,
            "bold must draw the glyph's ink twice: plain={}, bold={}",
            plain_rects.len(),
            bold_rects.len()
        );
        assert!(
            bold_width > plain_width,
            "bold must widen the measured advance: plain={plain_width}, bold={bold_width}"
        );
    }

    /// End-to-end colour control at [`push_entity_quads`] itself (not just
    /// the layout helper): a player/mob nametag carrying a `§c`-coloured
    /// span must reach the normal-pass vertex buffer with red, and its
    /// shadow copy must be a quarter-brightness *red* (`Font.java`'s
    /// `getShadowColor`'s `ARGB.scaleRGB(textColor, 0.25F)`), not the flat
    /// grey `SHADOW_COLOR` this pass drew for every name before this fix —
    /// see the module's `push_entity_quads` doc.
    #[test]
    fn push_entity_quads_resolves_a_coloured_nametag_span_and_its_shadow() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);
        let ink = StyledInkLayoutCache::default();

        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: std::sync::Arc::from("pig"),
            item: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            feet: Vec3::ZERO,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
            name_tag: Some(crate::entities::NameTag {
                text: Text::from_legacy("\u{a7}cA"),
                see_through: false,
            }),
            item_use: None,
            creeper_swelling: 0.0,
            swim_amount: 0.0,
            death_time: 0.0,
            on_fire: false,
            invisible: false,
            armor_stand: None,
            player_skin: None,
            variant_sheet: None,
            experience_orb_value: None,
            cape_sway: (0.0, 0.0, 0.0),
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(&raster, &ink, &draw, Vec3::ZERO, Vec3::X, Vec3::Y, &mut normal, &mut see_through);
        assert!(!normal.is_empty(), "a coloured, in-range named entity must still contribute normal-pass ink");

        let hex = TextColor::Red.rgb();
        let red_rgb = [
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
        ];
        let is_close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let has_red_at_alpha_one = normal
            .iter()
            .any(|v| is_close(v.color[0], red_rgb[0]) && is_close(v.color[1], red_rgb[1]) && is_close(v.color[2], red_rgb[2]) && is_close(v.color[3], 1.0));
        assert!(
            has_red_at_alpha_one,
            "the normal pass must draw the §c span in full-brightness red: {:?}",
            normal.iter().map(|v| v.color).collect::<Vec<_>>()
        );
        let shadow_rgb = [
            red_rgb[0] * metrics::SHADOW_BRIGHTNESS,
            red_rgb[1] * metrics::SHADOW_BRIGHTNESS,
            red_rgb[2] * metrics::SHADOW_BRIGHTNESS,
        ];
        let has_red_shadow = normal
            .iter()
            .any(|v| is_close(v.color[0], shadow_rgb[0]) && is_close(v.color[1], shadow_rgb[1]) && is_close(v.color[2], shadow_rgb[2]));
        assert!(
            has_red_shadow,
            "the shadow copy must be a quarter-brightness *red*, not flat \
             grey: wanted {shadow_rgb:?}, got {:?}",
            normal.iter().map(|v| v.color).collect::<Vec<_>>()
        );
    }

    /// **The hex-colour control.** `NameTag::text` is a real
    /// [`lodestone_model::text::Text`] now, and `push_entity_quads` reads it
    /// with `Text::to_spans` directly — but a *legacy*-expressible colour
    /// (like [`TextColor::Red`] above) cannot see the bug this guards, since
    /// `to_legacy_string`/`from_legacy` round-trips those losslessly. Only a
    /// [`TextColor::Rgb`] hex colour discriminates: legacy `§` codes are a
    /// 16-entry palette with no hex form at all.
    ///
    /// The control is run first, in this same test, reproducing the exact
    /// lossy path `NameTag::text` used to bridge through
    /// (`text.to_legacy_string()` then `Text::from_legacy(..).to_spans()`)
    /// and asserting it drops the hex colour — watched failing, not assumed
    /// — before asserting the real, direct `to_spans()` path
    /// `push_entity_quads` now takes preserves it end to end, to the drawn
    /// vertex colour.
    #[test]
    fn push_entity_quads_preserves_a_hex_nametag_colour_the_legacy_round_trip_cannot() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);
        let ink = StyledInkLayoutCache::default();

        let hex = 0x00FF_8800_u32;
        let mut hex_text = Text::literal("A");
        hex_text.style.color = Some(TextColor::Rgb(hex));

        // Control: the round trip `NameTag::text` used to be bridged
        // through, watched failing on the exact hypothesis it would have
        // produced.
        let lossy_spans = Text::from_legacy(&hex_text.to_legacy_string()).to_spans();
        assert!(
            lossy_spans
                .iter()
                .all(|s| s.style.color != Some(TextColor::Rgb(hex))),
            "control: a to_legacy_string/from_legacy round trip must lose a \
             hex colour (legacy `§` codes have no hex form) — this is the bug \
             `NameTag::text` used to have when it stored a plain `String`; \
             got {:?}",
            lossy_spans.iter().map(|s| s.style.color).collect::<Vec<_>>()
        );

        let draw = EntityDraw {
            hurt: false,
            id: 1,
            type_path: std::sync::Arc::from("pig"),
            item: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            feet: Vec3::ZERO,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            anim: lodestone_render::AnimInput::REST,
            wool: None,
            block_state: None,
            item_frame_rotation: 0,
            count: 1,
            foil: false,
            item_dyed_color: None,
            item_potion_color: None,
            name_tag: Some(crate::entities::NameTag {
                text: hex_text,
                see_through: false,
            }),
            item_use: None,
            creeper_swelling: 0.0,
            swim_amount: 0.0,
            death_time: 0.0,
            on_fire: false,
            invisible: false,
            armor_stand: None,
            player_skin: None,
            variant_sheet: None,
            experience_orb_value: None,
            cape_sway: (0.0, 0.0, 0.0),
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(&raster, &ink, &draw, Vec3::ZERO, Vec3::X, Vec3::Y, &mut normal, &mut see_through);

        let want_hex = [
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
        ];
        let is_close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let has_hex_at_alpha_one = normal.iter().any(|v| {
            is_close(v.color[0], want_hex[0])
                && is_close(v.color[1], want_hex[1])
                && is_close(v.color[2], want_hex[2])
                && is_close(v.color[3], 1.0)
        });
        assert!(
            has_hex_at_alpha_one,
            "the normal pass must draw the hex-coloured span in its real \
             colour {want_hex:?}, reached through `NameTag::text`'s direct \
             `to_spans()` (no legacy round trip): got {:?}",
            normal.iter().map(|v| v.color).collect::<Vec<_>>()
        );
    }
}
