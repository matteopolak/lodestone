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
//! # What each pass carries, and why the plate is in only one of them
//!
//! `SubmitNodeCollection.submitNameTag` makes **two different submissions**
//! depending on `!isDiscrete`, and the two differ in *three* things at once —
//! glyph colour, background colour and which group they land in. Transcribed
//! from that method rather than inferred from the symptom:
//!
//! | not discrete (the usual case) | discrete (sneaking) |
//! |---|---|
//! | `nameTags`: colour `-1` (opaque white), background `0` — **no plate** | `nameTags`: colour `-2130706433`, background `getBackgroundOpacity` — **plate** |
//! | `seeThroughNameTags`: colour `-2130706433` (`0x81_FFFFFF`, white at `129/255`), background `getBackgroundOpacity` — **plate** | *(nothing)* |
//!
//! So the plate is drawn exactly once per tag, in whichever group carries it,
//! and for a non-sneaking tag that is the **see-through** group — which
//! `FeatureRenderDispatcher.executeTranslucent` runs *first*, before the
//! depth-tested `nameTags` phase. [`NameTagRenderer::draw`] matches that, and
//! its own doc records why the earlier reading (the field declaration order
//! in `SubmitNodeCollection`) was the wrong source. The resulting composite
//! for a non-sneaking tag is the plate and a faded full-bright copy laid
//! down first, with the lit opaque copy painting over both wherever the tag
//! is not occluded — the familiar bright-name-on-dark-slab look, and behind a
//! wall only the faded copy survives. Both groups are plain
//! `BlendFunction.TRANSLUCENT` (`wgpu::BlendState::ALPHA_BLENDING` here).
//!
//! # Light: only one of the two groups samples a lightmap
//!
//! `text.vsh` is compiled per render type, and its `IS_SEE_THROUGH` variant
//! declares no `UV2` input at all: that branch is `vertexColor = Color`,
//! while the plain branch is `vertexColor = Color * sample_lightmap(Sampler2,
//! UV2)`. `text_background.vsh` forks identically. So a see-through name tag
//! is full-bright in vanilla **by construction**, however its `lightCoords`
//! argument was computed, and only the depth-tested group takes the world's
//! light.
//!
//! That group's light is not the raw sample either:
//! `SubmitNodeCollection.submitNameTag` passes
//! `LightCoordsUtil.lightCoordsWithEmission(lightCoords, 2)` for the
//! non-discrete case — both halves floored at 2 — and the raw coords for the
//! discrete (sneaking) one, which is a second thing that submission carries
//! and the see-through one does not. [`WorldTextLight`] turns the resulting
//! byte into vanilla's own lightmap texel via
//! [`lodestone_render::light_color`], on the CPU, because this pass's entire
//! shader input is one flat vertex colour and vanilla's own multiply is in
//! its vertex stage too.
//!
//! **The plate is drawn before the glyphs within its own group**, matching
//! `Font.PreparedTextBuilder.visit`, which emits the background effect first
//! and every glyph after it. Vanilla additionally pushes the plate `-0.01`
//! along local `z`; nothing here does, and nothing needs to — a nametag
//! billboard's plane is perpendicular to the view axis, so every vertex in it
//! shares one window depth and the depth test cannot separate the plate from
//! the glyphs at all. Submission order is what separates them, and
//! `LessEqual` passes the resulting tie. (Porting the `-0.01` faithfully
//! would also be inert: through the `0.025` text scale it is `0.00025`
//! blocks, which is 0–2 ULP of `Depth32Float` at any real viewing distance
//! under this project's forward `[0,1]` projection. See `docs/shaders.md`
//! and CLAUDE.md's rendering constraints.)
//!
//! # The plate's alpha is a linear-blend divergence, not a wrong constant
//!
//! The plate colour is black at `getBackgroundOpacity(0.25F)`, and that
//! accessor returns its **fallback** `0.25` unless `backgroundForChatOnly`
//! is turned off — an option vanilla defaults *on* and this crate does not
//! model at all, exactly as `gpu/display_text.rs`'s `DEFAULT_BACKGROUND_ARGB`
//! already records for the same accessor. So `0x40000000` is the faithful
//! value for an unconfigured client, and `Options::chat_background_opacity`
//! (this crate's chat-HUD slider) is deliberately **not** threaded in here:
//! vanilla's chat-only default means it would not feed this value either.
//!
//! What *was* a real divergence, and is fixed structurally rather than by
//! touching that constant: vanilla composites this plate on raw gamma bytes,
//! while every pipeline in this crate targets the swapchain's **sRGB** view, so
//! the hardware blended in linear light — the same colour-space mismatch
//! `docs/tab-list.md` records for the HUD's flat-colour stream. For a
//! pure-black source the two agree only at a black backdrop (white is *not* a
//! second fixed point, unlike the tab-list case) and diverge monotonically
//! toward it: re-derived from the sRGB transfer function, `0.75·bg` (vanilla)
//! against `encode(0.75·decode(bg))` (the bug) is `0` at `bg = 0`, ≈+7/255 at
//! `bg = 64`, ≈+16/255 at `bg = 128` and ≈+33/255 at `bg = 255`, i.e. the plate
//! read **too weak against a bright backdrop**.
//!
//! This pass therefore draws into a **raw** (non-sRGB) view of the same colour
//! texture, in its own render pass — a `wgpu` render pass fixes one attachment
//! format for every pipeline in it, so there is no way to have one pipeline in
//! the block pass blend differently from its neighbours.
//! `RenderState::set_world_text_view` installs the view and re-points these
//! pipelines' format from one expression, so the two cannot be decided apart.
//! `gpu/sign_text.rs` and `gpu/display_text.rs` had the identical exposure —
//! all three share this file's shader, so every colour any of them submits is a
//! vanilla gamma byte — and moved with it. See
//! `docs/world-text-gamma-blend.md`.
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
use std::sync::Arc;

use glam::Vec3;
use lodestone_assets::font::{FontLoader, FontOptions, GlyphRaster, MISSING_ADVANCE, RasterFont, metrics};
use lodestone_assets::{ResourceManager, ResourceSource, ZipSource};
use lodestone_model::text::{FontId, Text, TextColor, TextSpan};
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

/// `LightCoordsUtil.FULL_BRIGHT` (`15728880` = sky 15, block 15) in this
/// renderer's one-byte `sky << 4 | block` layout.
///
/// **Not** [`lodestone_render::ENTITY_FULLBRIGHT`], which is `15 << 4` — sky
/// 15 and block **0**. That one is a *fallback* for a caller with no world to
/// sample and still dims with the clock, because `sky_darken` scales the sky
/// half. Vanilla's `FULL_BRIGHT` sets both halves, and block light does not
/// dim at dusk, so this byte is the only one that means "bright at midnight
/// too" — which is exactly what glowing sign text is for. The same
/// distinction `gpu/entity_passes.rs::framed_content_light` already spells as
/// `ENTITY_FULLBRIGHT | 0x0F` for a glow item frame's contents.
pub(super) const TEXT_FULL_BRIGHT: u8 = 0xFF;

/// This frame's two world-scoped lightmap inputs, so a pass that turns a
/// packed light byte into a tint carries one value rather than two loose
/// floats.
///
/// Both are per-*frame*, not per-draw: `sky_darken` is the clock's
/// `SKY_LIGHT_FACTOR` and `ambient` is the current dimension's
/// `AMBIENT_LIGHT_COLOR`. `RenderState` already polls both once a frame for
/// the terrain and entity passes (`SkyDarkenSource`/`AmbientLightSource`);
/// this is the seam that hands the same two numbers to the three flat-colour
/// world-text passes, which had been sampling no lightmap at all.
#[derive(Debug, Clone, Copy)]
pub(super) struct WorldTextLight {
    sky_darken: f32,
    ambient: [f32; 3],
}

impl WorldTextLight {
    /// From this frame's already-polled sources — see the type's own doc.
    pub(super) fn new(sky_darken: f32, ambient: [f32; 3]) -> Self {
        Self { sky_darken, ambient }
    }

    /// Overworld noon: `sky_darken = 1.0` and the dimension's default ambient
    /// colour, which is what `SkyDarkenSource`/`AmbientLightSource` resolve to
    /// with no world attached.
    ///
    /// Every gate in these three passes that predates world-text lighting is
    /// about *colour resolution* or *layout*, so each takes this plus a
    /// full-bright packed byte and keeps asserting exactly what it always
    /// asserted. The gates that are about the light say so in their names and
    /// pass a real byte instead — a helper that made the tint unconditionally
    /// `[1, 1, 1]` would have made those unfalsifiable.
    #[cfg(test)]
    pub(super) fn overworld_noon() -> Self {
        Self::new(1.0, lodestone_render::light::OVERWORLD_AMBIENT_LIGHT)
    }

    /// Vanilla's lightmap texel for `packed`, as an RGB multiplier.
    ///
    /// This is [`lodestone_render::light_color`] and nothing else — the one
    /// authority `shaders/model.wgsl`, `shaders/entity.wgsl` and
    /// `shaders/fluid.wgsl` all duplicate. It is resolved on the CPU here
    /// rather than in `shaders/nametag.wgsl` because that shader's whole
    /// input is one flat vertex colour: vanilla's `text.vsh` does
    /// `vertexColor = Color * sample_lightmap(Sampler2, UV2)` in its *vertex*
    /// stage too, so folding the texel into the colour before upload is the
    /// same arithmetic at the same rate, with no new vertex attribute, no
    /// second uniform and no lightmap texture to keep in sync.
    ///
    /// The multiply is a plain one in **gamma** space, with no sRGB
    /// round-trip: vanilla is not colour-managed, and these three passes draw
    /// into the target's raw (non-sRGB) view for exactly that reason — see
    /// the module doc's colour-space section.
    #[must_use]
    pub(super) fn tint(self, packed: u8) -> [f32; 3] {
        lodestone_render::light_color(packed, self.sky_darken, self.ambient)
    }
}

/// `color` with `tint` multiplied into its RGB and its alpha untouched —
/// `text.vsh`'s `Color * sample_lightmap(...)`, whose alpha channel the
/// lightmap texture leaves at `1.0`.
#[must_use]
pub(super) fn tinted(color: [f32; 4], tint: [f32; 3]) -> [f32; 4] {
    [color[0] * tint[0], color[1] * tint[1], color[2] * tint[2], color[3]]
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

/// Opaque white — vanilla's `-1`, and the colour of **only** the
/// non-discrete normal-pass submission (`SubmitNodeCollection.submitNameTag`).
/// Only its alpha (`1.0`) is read: the RGB half of a `StyledRect` is
/// already resolved (white when a span's own colour is unspecified, real
/// per-span colour otherwise) by [`layout_styled_ink_runs`] — see the module
/// doc's table for which submission takes this and which takes
/// [`SEE_THROUGH_COLOR`].
const NORMAL_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// White at `129/255`, vanilla's `-2130706433` (`0x81_FFFFFF`). Same "alpha
/// only" reading as [`NORMAL_COLOR`]. Carried by the see-through submission
/// **and** by the discrete (sneaking) normal-pass one — the module doc's
/// table; reading this as "the see-through pass's colour" is what made a
/// sneaking tag draw at full opacity where vanilla fades it.
const SEE_THROUGH_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 129.0 / 255.0];

/// The background plate's packed ARGB: black at
/// `Options.getBackgroundOpacity(0.25F)`, which resolves to its `0.25`
/// fallback on an unconfigured client — `ARGB.color(0.25F, -16777216)` =
/// `as8BitChannel(0.25) << 24` = `64 << 24`. See the module doc for why the
/// option is not read here, and for the linear-blend divergence this value
/// is *not* responsible for.
///
/// The same number as `gpu/display_text.rs`'s `DEFAULT_BACKGROUND_ARGB`? No —
/// that one is `0x3F000000`, one step darker, because it rounds the same
/// `0.25` through `(int)(x * 255)` truncation rather than `as8BitChannel`'s
/// `Math.round`. Two accessors, one off by one; folding them would be
/// invisible.
const BACKGROUND_ARGB: i32 = 0x4000_0000_u32 as i32;

/// The plate's padding left of the pen and above the line's top edge, in
/// logical font pixels — `Font.PreparedTextBuilder.markBackground`'s
/// `x - 1.0F` / `y - 1.0F`. Its right edge is the line's full advance and its
/// bottom edge is `y + 9.0F` ([`metrics::LINE_HEIGHT`]), with no padding on
/// either, so the rect is deliberately **not** symmetric.
const BACKGROUND_PAD: f32 = 1.0;

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
    /// How far this rect would be displaced by one step of vanilla's 8×
    /// text outline — `GlyphInfo.getShadowOffset()` for a run of glyph ink
    /// (1 px for a sheet glyph, 0.5 for a unihex one), and **`0.0` for an
    /// underline/strikethrough bar or a background plate**, because
    /// `Font.prepare8xTextOutline` ends in `outlineOutput.discardEffects()`
    /// and so outlines the glyphs only.
    ///
    /// One field rather than a `glyph: bool` plus a separate offset: the two
    /// facts are the same fact, and vanilla spells them as one call that
    /// simply never happens for an effect bar. Only `gpu/sign_text.rs`'s
    /// glowing-sign outline reads it; the other two consumers draw every rect
    /// identically and ignore it.
    pub(super) outline_grow: f32,
}

/// The raster selected for one component glyph. A borrowed default font keeps
/// the ordinary no-pack path allocation-free; a shared value keeps a lazily
/// loaded resource-pack font alive for the whole glyph walk.
pub(super) enum ResolvedRaster<'a> {
    Borrowed(&'a RasterFont),
    Shared(Arc<RasterFont>),
}

impl AsRef<RasterFont> for ResolvedRaster<'_> {
    fn as_ref(&self) -> &RasterFont {
        match self {
            Self::Borrowed(raster) => raster,
            Self::Shared(raster) => raster,
        }
    }
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
    resolved: std::sync::Mutex<std::collections::HashMap<(u64, Vec<TextSpan>), StyledInkLayout>>,
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

    /// The resource-pack-aware form of [`Self::layout`]. A selected font can
    /// change on a pack reload even when the component text is unchanged, so
    /// the generation is part of this cache's identity rather than a global
    /// clear that could race a frame still using the prior font snapshot.
    pub(super) fn layout_resolved<'a, F>(
        &self,
        generation: u64,
        spans: &[TextSpan],
        select: F,
    ) -> StyledInkLayout
    where
        F: FnMut(u32, Option<FontId>) -> ResolvedRaster<'a>,
    {
        let mut map = self
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (generation, spans.to_vec());
        if let Some(hit) = map.get(&key) {
            return std::sync::Arc::clone(hit);
        }
        if map.len() >= Self::MAX_ENTRIES {
            map.clear();
        }
        let layout: StyledInkLayout = std::sync::Arc::new(layout_styled_ink_runs_resolved(spans, select));
        map.insert(key, std::sync::Arc::clone(&layout));
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
/// The **advance width** of a styled span list, in logical pixels — the same
/// number [`layout_styled_ink_runs`] returns as its second element, without
/// walking a single texel.
///
/// This is vanilla's `StringSplitter` measure rather than its `Font` draw:
/// `new StringSplitter((codepoint, style) -> ...getGlyph(codepoint).info()
/// .getAdvance(style.isBold()))`, the measure `Font.split` wraps against. It
/// exists because a wrap calls its measure once per candidate row, and
/// rasterising a `ttf` glyph (`RasterFont::raster` bakes on demand) to learn a
/// number that lives on `GlyphInfo` would make wrapping cost more than
/// drawing.
///
/// `GlyphRaster::advance` and `Glyph::advance` are the same three arms over
/// the same three glyph kinds, so this cannot disagree with the layout's own
/// cursor — `the_cheap_advance_measure_agrees_with_the_full_ink_walk` is the
/// gate that keeps it that way, because nothing in the type system would.
pub(super) fn styled_advance_width(raster: &RasterFont, spans: &[TextSpan]) -> f32 {
    let font = raster.font();
    let mut cursor = 0.0f32;
    for span in spans {
        let bold = span.style.bold.unwrap_or(false);
        for ch in span.text.chars() {
            let cp = ch as u32;
            cursor += font.advance(cp).unwrap_or(MISSING_ADVANCE)
                + if bold { font.bold_offset(cp) } else { 0.0 };
        }
    }
    cursor
}

pub(super) fn layout_styled_ink_runs(raster: &RasterFont, spans: &[TextSpan]) -> (Vec<StyledRect>, f32) {
    layout_styled_ink_runs_resolved(spans, |_, _| ResolvedRaster::Borrowed(raster))
}

/// [`layout_styled_ink_runs`] with vanilla's per-span font selection supplied
/// by the caller. A selected font wins only for codepoints it actually covers;
/// callers return the default raster for the ordinary fallback case.
pub(super) fn layout_styled_ink_runs_resolved<'a, F>(
    spans: &[TextSpan],
    mut select: F,
) -> (Vec<StyledRect>, f32)
where
    F: FnMut(u32, Option<FontId>) -> ResolvedRaster<'a>,
{
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
            let selected = select(cp, span.style.font);
            let raster = selected.as_ref();
            let x0 = cursor;
            let glyph_raster = raster.raster(cp);
            let base_advance = glyph_raster
                .as_ref()
                .map_or_else(|| raster.advance(cp).unwrap_or(MISSING_ADVANCE), GlyphRaster::advance);
            let bold_extra = raster.font().bold_offset(cp);
            // `GlyphInfo.getShadowOffset()` — carried onto every ink rect this
            // glyph emits so a consumer can reproduce vanilla's 8× outline
            // without re-resolving the glyph. Half a pixel for unihex, one for
            // everything else, per glyph rather than per font.
            let shadow_extra = raster.font().shadow_offset(cp);
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
                        let source = r.texel_rgba(tx, ty);
                        while tx < r.cell_width()
                            && r.texel_rgba(tx, ty) == source
                            && source[3] != 0.0
                        {
                            tx += 1;
                        }
                        let shear = if italic {
                            let v = top + (ty as f32 + 0.5) * texel;
                            font_shear(v)
                        } else {
                            0.0
                        };
                        let color = [
                            color[0] * source[0],
                            color[1] * source[1],
                            color[2] * source[2],
                            color[3] * source[3],
                        ];
                        let rx = x0 + left + shear + run_start as f32 * texel;
                        let ry = top + ty as f32 * texel;
                        let rw = (tx - run_start) as f32 * texel;
                        rects.push(StyledRect {
                            x: rx,
                            y: ry,
                            w: rw,
                            h: texel,
                            color,
                            outline_grow: shadow_extra,
                        });
                        if bold {
                            rects.push(StyledRect {
                                x: rx + bold_extra,
                                y: ry,
                                w: rw,
                                h: texel,
                                color,
                                outline_grow: shadow_extra,
                            });
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
                        outline_grow: 0.0,
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
                        outline_grow: 0.0,
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
#[allow(clippy::too_many_arguments)]
fn push_entity_quads(
    raster: &RasterFont,
    ink: &StyledInkLayoutCache,
    draw: &EntityDraw,
    camera_position: Vec3,
    right: Vec3,
    up: Vec3,
    light: WorldTextLight,
    light_source: &super::EntityLightSource,
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

    // The plate, in the same local logical-pixel space the ink runs are laid
    // out in. `Font.PreparedTextBuilder.markBackground` seeds the rect at the
    // pen (`x - 1`, `y - 1`, `x + 0`, `y + 9`) and grows its right edge by
    // each glyph's advance; this layout starts its pen at local `(0, 0)`, so
    // the finished rect is `(-1, -1)` to `(total_width, 9)`.
    let plate = StyledRect {
        x: -BACKGROUND_PAD,
        y: -BACKGROUND_PAD,
        w: total_width + BACKGROUND_PAD,
        h: metrics::LINE_HEIGHT + BACKGROUND_PAD,
        color: lodestone_render::display::text_background_color(BACKGROUND_ARGB),
        outline_grow: 0.0,
    };
    // The tag's own packed light, exactly as every other entity pass resolves
    // it (eye-height probe, fire forcing the block half) — `submitNameTag`
    // takes `state.lightCoords`, the same field `LivingEntityRenderer` hands
    // its model.
    //
    // The **normal** group's non-discrete submission takes
    // `LightCoordsUtil.lightCoordsWithEmission(lightCoords, 2)`, which floors
    // *both* halves at 2, so an unoccluded name never goes fully black in a
    // pitch-dark cave. The discrete (sneaking) submission takes the raw
    // coords. See-through gets no tint at all — see `see_through_tint` below.
    let packed = super::entity_passes::entity_light(light_source, draw);
    let normal_tint = light.tint(if tag.see_through {
        light_coords_with_emission(packed, 2)
    } else {
        packed
    });
    // `RenderTypes.textSeeThrough`'s shader is `text.vsh` compiled with
    // `IS_SEE_THROUGH`, and that branch is literally `vertexColor = Color` —
    // it declares no `UV2` input and samples no lightmap, so a see-through
    // name tag is full-bright in vanilla *by construction*, whatever
    // `lightCoords` it was submitted with. Reading the submission's light
    // argument rather than the shader it selects is what would make this
    // range double-dim.
    let see_through_tint = [1.0, 1.0, 1.0];
    let plate_quad = |out: &mut Vec<NameTagVertex>, tint: [f32; 3]| {
        // `text_background.vsh` forks on `IS_SEE_THROUGH` the same way its
        // glyph sibling does, so the plate takes whichever tint its own group
        // takes.
        out.extend(quad_vertices(
            plate,
            half_width,
            anchor,
            right,
            up,
            tinted(plate.color, tint),
        ));
    };
    let glyph_quads = |out: &mut Vec<NameTagVertex>, alpha: f32, tint: [f32; 3]| {
        for rect in rects {
            // Only alpha comes from the pass; the RGB is the span's own
            // resolved colour, which `layout_styled_ink_runs` deliberately
            // leaves at alpha `1.0` so one cached layout can serve every
            // pass at a different opacity.
            let color = [rect.color[0], rect.color[1], rect.color[2], alpha];
            out.extend(quad_vertices(
                *rect,
                half_width,
                anchor,
                right,
                up,
                tinted(color, tint),
            ));
        }
    };

    // No drop shadow, in either branch: `NameTagFeatureRenderer.prepareText`
    // calls `Font.prepareText(..., drawShadow = false, ...)`, and
    // `Font.PreparedTextBuilder.getShadowColor` returns a fully transparent
    // `0` for a style carrying no explicit shadow colour when that flag is
    // clear, so vanilla emits no shadow renderable at all. The plate is what
    // separates a nametag from the world behind it; this pass used to draw a
    // quarter-brightness shadow copy *instead of* the plate, which is the
    // thing that made a name read as bare text.
    if tag.see_through {
        // Not discrete. The depth-tested submission is opaque white with a
        // background colour of literally `0`; the plate travels with the
        // faded see-through copy. `NameTagRenderer::draw` records the
        // see-through group **first** and the depth-tested one over it, which
        // is the order `FeatureRenderDispatcher.executeTranslucent` runs the
        // two phases in — see that method's doc for why the field
        // declaration order in `SubmitNodeCollection` is not it.
        glyph_quads(normal_out, NORMAL_COLOR[3], normal_tint);
        plate_quad(see_through_out, see_through_tint);
        glyph_quads(see_through_out, SEE_THROUGH_COLOR[3], see_through_tint);
    } else {
        // Discrete (sneaking): one submission only, to the depth-tested
        // group, and it carries both the plate and the faded glyph alpha.
        plate_quad(normal_out, normal_tint);
        glyph_quads(normal_out, SEE_THROUGH_COLOR[3], normal_tint);
    }
}

/// `LightCoordsUtil.lightCoordsWithEmission`, in this renderer's one-byte
/// layout: each half of `packed` floored at `emission` independently.
///
/// Vanilla's is `pack(max(block, e), max(sky, e))` on a 32-bit light
/// coordinate; here the halves are the two nibbles of a byte. The only caller
/// is the non-discrete name tag's depth-tested submission, at `e = 2`.
#[must_use]
fn light_coords_with_emission(packed: u8, emission: u8) -> u8 {
    let sky = ((packed >> 4) & 0x0F).max(emission);
    let block = (packed & 0x0F).max(emission);
    (sky << 4) | block
}

/// Draws billboarded nametag text above every [`EntityDraw`] carrying one —
/// see the module doc for the two depth passes' exact settings.
#[derive(Debug)]
pub(super) struct NameTagRenderer {
    normal_pipeline: wgpu::RenderPipeline,
    see_through_pipeline: wgpu::RenderPipeline,
    /// Kept so [`NameTagRenderer::set_color_format`] can rebuild both
    /// pipelines against the *same* layout object [`Self::bind_group`] was
    /// created from. Rebuilding it from an identical descriptor instead would
    /// rely on `wgpu` deduplicating bind-group layouts by content, which is an
    /// implementation detail rather than a promise.
    bind_layout: wgpu::BindGroupLayout,
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

        let (normal_pipeline, see_through_pipeline) =
            build_pipelines(device, &bind_layout, color_format);

        Self {
            normal_pipeline,
            see_through_pipeline,
            bind_layout,
            bind_group,
            uniform,
            normal_vertices,
            see_through_vertices,
            font: load_font(),
            ink: StyledInkLayoutCache::default(),
        }
    }

    /// Rebuild both pipelines for a colour attachment of `color_format`,
    /// keeping the font, the ink cache and every buffer this pass already
    /// filled.
    ///
    /// A pipeline's colour target format has to equal its render pass
    /// attachment's, and which attachment this pass gets is not known when
    /// [`RenderState`](crate::gpu::RenderState) is built: a nametag composites
    /// on **raw gamma bytes** (see the module doc), so on an sRGB target it
    /// draws into a non-sRGB *view* of the same swapchain texture, and only
    /// the frame loop can hand that view over. `RenderState::set_world_text_view`
    /// is the one caller, and it derives the format and the view from the same
    /// place so the two cannot disagree.
    pub(super) fn set_color_format(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
    ) {
        let (normal, see_through) = build_pipelines(device, &self.bind_layout, color_format);
        self.normal_pipeline = normal;
        self.see_through_pipeline = see_through;
    }
}

/// Both nametag pipelines for a colour attachment of `color_format`. Shared by
/// [`NameTagRenderer::new`] and [`NameTagRenderer::set_color_format`] so the
/// depth states below are written once — a second copy would be free to drift
/// from vanilla's two records without anything going red.
fn build_pipelines(
    device: &wgpu::Device,
    bind_layout: &wgpu::BindGroupLayout,
    color_format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::RenderPipeline) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("lodestone-nametag-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/nametag.wgsl").into()),
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("lodestone-nametag-layout"),
        bind_group_layouts: &[Some(bind_layout)],
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

    (normal_pipeline, see_through_pipeline)
}

impl NameTagRenderer {
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
        light: WorldTextLight,
        light_source: &super::EntityLightSource,
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
                light,
                light_source,
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
    ///
    /// # The see-through group draws FIRST
    ///
    /// `FeatureRenderDispatcher.executeTranslucent` runs
    /// `executePhase(collection.seeThroughNameTags)` and *then*
    /// `executePhase(collection.nameTags)`. This file used to record the
    /// opposite order, citing the field **declaration** order in
    /// `SubmitNodeCollection` (`nameTags` then `seeThroughNameTags`) — the
    /// same trap as reading a packet's wire order off its record's field
    /// list rather than off its `write` method. The dispatcher is the one
    /// that runs.
    ///
    /// It is not cosmetic, and it is what makes name-tag lighting
    /// load-bearing. The see-through copy is un-depth-tested, full-bright by
    /// construction (see [`push_entity_quads`]'s `see_through_tint`) and
    /// carries the plate; the depth-tested copy is opaque white multiplied by
    /// the real lightmap. Drawn in vanilla's order the lit copy wins wherever
    /// the tag is unoccluded, so a name in a dark room reads dark. Drawn the
    /// other way round a 129/255 full-bright copy paints over the lit one and
    /// roughly half the darkening is thrown away — the light would reach the
    /// vertex buffer and mostly not reach the screen.
    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, counts: (u32, u32)) {
        let (normal_count, see_through_count) = counts;
        if see_through_count > 0 {
            pass.set_pipeline(&self.see_through_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.see_through_vertices.slice(..));
            pass.draw(0..see_through_count, 0..1);
        }
        if normal_count > 0 {
            pass.set_pipeline(&self.normal_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.normal_vertices.slice(..));
            pass.draw(0..normal_count, 0..1);
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
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
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
            painting: None,
            firework: None,
            projectile_owner: None,
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
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
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
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
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
            painting: None,
            firework: None,
            projectile_owner: None,
        };
        push_entity_quads(
            &raster,
            &StyledInkLayoutCache::default(),
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
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
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
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
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
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
            painting: None,
            firework: None,
            projectile_owner: None,
        };
        push_entity_quads(
            &raster,
            &StyledInkLayoutCache::default(),
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
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

    /// [`styled_advance_width`] and [`layout_styled_ink_runs`]'s own returned
    /// cursor must be the *same number*, because `gpu/sign_text.rs` wraps
    /// against the cheap one and then centres against the expensive one — a
    /// disagreement puts every truncated sign line off-centre by the
    /// difference, silently.
    ///
    /// The fixture is the `pixel_scale = 0.625` one, deliberately: at
    /// `pixel_scale == 1.0` a cell width and an advance coincide for these
    /// glyphs, so the *shared* blind spot of every other font fixture in this
    /// file would let a cell-width-based measure pass. Bold is asserted
    /// separately from plain because the bold extra advance is the one term
    /// the two implementations reach by different routes
    /// (`Font::bold_offset` here, the same call inline there).
    #[test]
    fn the_cheap_advance_measure_agrees_with_the_full_ink_walk() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);

        for bold in [false, true] {
            let spans = vec![
                TextSpan {
                    text: "AB".to_string(),
                    style: lodestone_model::text::TextStyle {
                        bold: Some(bold),
                        ..Default::default()
                    },
                },
                TextSpan {
                    text: "BA".to_string(),
                    style: lodestone_model::text::TextStyle::default(),
                },
            ];
            let walked = layout_styled_ink_runs(&raster, &spans).1;
            let measured = styled_advance_width(&raster, &spans);
            assert!(
                (walked - measured).abs() < 1e-4,
                "bold {bold}: ink walk {walked}, cheap measure {measured}"
            );
            // Not vacuous: the two must actually have measured something, and
            // the bold arm must differ from the plain one.
            assert!(walked > 0.0, "bold {bold}: measured nothing at all");
        }
        let plain = vec![TextSpan {
            text: "AB".to_string(),
            style: lodestone_model::text::TextStyle::default(),
        }];
        let bolded = vec![TextSpan {
            text: "AB".to_string(),
            style: lodestone_model::text::TextStyle {
                bold: Some(true),
                ..Default::default()
            },
        }];
        assert!(
            styled_advance_width(&raster, &bolded) > styled_advance_width(&raster, &plain),
            "bold must measure wider, or this gate cannot see the bold term at all"
        );
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

    #[test]
    fn resolved_layout_uses_a_pack_glyph_and_modulates_its_native_rgba() {
        let default = scaled_raster("AB", 1, 1, &[255, 255, 255, 255, 255, 255, 255, 255]);
        // The pack glyph has two adjacent, differently coloured/translucent
        // texels. A uniform component tint or one scanline run cannot produce
        // this result.
        let pack = scaled_raster(
            "A",
            2,
            1,
            &[
                128, 64, 32, 128, 32, 128, 255, 64, // distinct adjacent ink texels
                0, 0, 0, 0, 0, 0, 0, 0, // transparent second row
            ],
        );
        let font = lodestone_model::text::FontId::intern("nameplates:default");
        let spans = vec![TextSpan {
            text: "AB".to_owned(),
            style: lodestone_model::text::TextStyle {
                color: Some(TextColor::Rgb(0x0000_ff)),
                font: Some(font),
                ..Default::default()
            },
        }];

        let (rects, _) = layout_styled_ink_runs_resolved(&spans, |codepoint, requested| {
            if requested == Some(font) && codepoint == 'A' as u32 {
                ResolvedRaster::Borrowed(&pack)
            } else {
                ResolvedRaster::Borrowed(&default)
            }
        });

        assert_eq!(rects.len(), 3, "the pack glyph's adjacent multicolour texels must not merge");
        assert_eq!(rects[0].color, [0.0, 0.0, 32.0 / 255.0, 128.0 / 255.0]);
        assert_eq!(rects[1].color, [0.0, 0.0, 1.0, 64.0 / 255.0]);
        assert_eq!(rects[2].color, [0.0, 0.0, 1.0, 1.0], "a codepoint absent from the selected pack font must fall back to default");
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
    /// span must reach the vertex buffer with red rather than the plain
    /// white fallback.
    ///
    /// The subject is a **sneaking** tag (`see_through: false`), so vanilla's
    /// single discrete submission puts both the plate and the glyphs in the
    /// normal pass at [`SEE_THROUGH_COLOR`]'s `129/255` — asserted here
    /// exactly, because the wrong hypothesis (this pass's earlier behaviour:
    /// opaque white alpha for *every* normal-pass tag) differs only in that
    /// one number and nothing else in the buffer would show it.
    #[test]
    fn push_entity_quads_resolves_a_coloured_nametag_span_at_the_discrete_alpha() {
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
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
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
            painting: None,
            firework: None,
            projectile_owner: None,
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(
            &raster,
            &ink,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
            &mut normal,
            &mut see_through,
        );
        assert!(!normal.is_empty(), "a coloured, in-range named entity must still contribute normal-pass ink");

        let hex = TextColor::Red.rgb();
        let red_rgb = [
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
        ];
        let is_close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let has_red_at_discrete_alpha = normal.iter().any(|v| {
            is_close(v.color[0], red_rgb[0])
                && is_close(v.color[1], red_rgb[1])
                && is_close(v.color[2], red_rgb[2])
                && is_close(v.color[3], SEE_THROUGH_COLOR[3])
        });
        assert!(
            has_red_at_discrete_alpha,
            "a sneaking tag's normal pass must draw the §c span in red at \
             alpha {} (vanilla's -2130706433), not at the opaque white \
             alpha this pass used to give every normal-pass tag: {:?}",
            SEE_THROUGH_COLOR[3],
            normal.iter().map(|v| v.color).collect::<Vec<_>>()
        );
        // The wrong hypothesis, asserted absent rather than merely not
        // looked for: no glyph vertex may carry the old opaque alpha. The
        // plate is black, so restricting this to red-carrying vertices keeps
        // it about the glyphs.
        let opaque_red = normal.iter().any(|v| {
            is_close(v.color[0], red_rgb[0]) && is_close(v.color[3], 1.0)
        });
        assert!(
            !opaque_red,
            "no glyph vertex of a sneaking tag may be opaque: {:?}",
            normal.iter().map(|v| v.color).collect::<Vec<_>>()
        );
        // And no quarter-brightness copy of it: vanilla's nametags pass
        // `drawShadow = false`, so a shadow renderable is never emitted.
        let shadow_rgb = [
            red_rgb[0] * metrics::SHADOW_BRIGHTNESS,
            red_rgb[1] * metrics::SHADOW_BRIGHTNESS,
            red_rgb[2] * metrics::SHADOW_BRIGHTNESS,
        ];
        let has_shadow_copy = normal.iter().any(|v| {
            is_close(v.color[0], shadow_rgb[0])
                && is_close(v.color[1], shadow_rgb[1])
                && is_close(v.color[2], shadow_rgb[2])
        });
        assert!(
            !has_shadow_copy,
            "a nametag must emit no drop-shadow copy ({shadow_rgb:?}) — the \
             plate is what separates it from the world: {:?}",
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
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
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
            painting: None,
            firework: None,
            projectile_owner: None,
        };
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(
            &raster,
            &ink,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
            &mut normal,
            &mut see_through,
        );

        let want_hex = [
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
        ];
        let is_close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        // `see_through: false` above, so this is vanilla's discrete
        // submission: normal pass, glyphs at `129/255`.
        let has_hex = normal.iter().any(|v| {
            is_close(v.color[0], want_hex[0])
                && is_close(v.color[1], want_hex[1])
                && is_close(v.color[2], want_hex[2])
                && is_close(v.color[3], SEE_THROUGH_COLOR[3])
        });
        assert!(
            has_hex,
            "the normal pass must draw the hex-coloured span in its real \
             colour {want_hex:?}, reached through `NameTag::text`'s direct \
             `to_spans()` (no legacy round trip): got {:?}",
            normal.iter().map(|v| v.color).collect::<Vec<_>>()
        );
    }

    /// A named pig at the origin, carrying `text` and the given discreteness
    /// — the shape [`crate::entities::entity_facts`] hands the render list
    /// for a mob whose `CUSTOM_NAME` is set and whose `CUSTOM_NAME_VISIBLE`
    /// metadata is `true`. Nothing about the plate is data-driven, so this is
    /// the whole of the production input the plate gates below need; that
    /// resolution step has its own gates in `crate::entities`' `name_tag`
    /// module.
    fn named_pig(text: Text, see_through: bool) -> EntityDraw {
        EntityDraw {
            hurt: false,
            id: 1,
            type_path: std::sync::Arc::from("pig"),
            item: None,
            item_model: None,
            item_skin: None,
            main_arm_left: false,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
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
            name_tag: Some(crate::entities::NameTag { text, see_through }),
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
            painting: None,
            firework: None,
            projectile_owner: None,
        }
    }

    /// The plate's own colour as [`push_entity_quads`] emits it.
    fn plate_color() -> [f32; 4] {
        lodestone_render::display::text_background_color(BACKGROUND_ARGB)
    }

    /// Inverts [`quad_vertices`]' world placement back to the local
    /// logical-pixel space the plate rect is derived in, for the `right =
    /// +X` / `up = +Y` basis every gate here passes.
    fn to_local(v: &NameTagVertex, anchor: Vec3, half_width: f32) -> (f32, f32) {
        let p = Vec3::from(v.position) - anchor;
        (p.x / PX_SCALE + half_width, -p.y / PX_SCALE)
    }

    /// **The plate gate.** A non-sneaking named mob must draw a background
    /// plate, and vanilla puts it in a specific place: the depth-tested
    /// submission carries `backgroundColor = 0` and the see-through one
    /// carries the real colour (`SubmitNodeCollection.submitNameTag`, the
    /// module doc's table). So the discriminating assertion is not "a plate
    /// exists somewhere" — it is that the normal pass has **none** and the
    /// see-through pass leads with exactly one, before its glyphs.
    ///
    /// Uses the synthetic jar-free raster so this runs in CI regardless of
    /// whether a real client jar is present.
    #[test]
    fn a_visible_nametag_draws_its_plate_in_the_see_through_pass_only() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);
        let ink = StyledInkLayoutCache::default();

        let draw = named_pig(Text::from_legacy("AB"), true);
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(
            &raster,
            &ink,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
            &mut normal,
            &mut see_through,
        );

        let want = plate_color();
        assert_eq!(want, [0.0, 0.0, 0.0, 64.0 / 255.0], "the plate is black at 64/255");

        assert!(
            !normal.iter().any(|v| v.color == want),
            "vanilla's non-discrete depth-tested submission carries \
             `backgroundColor = 0`, so the normal pass must contain no plate"
        );
        assert!(
            see_through.len() >= 6,
            "the see-through pass must carry a plate quad plus glyphs, got {} vertices",
            see_through.len()
        );
        assert!(
            see_through[..6].iter().all(|v| v.color == want),
            "the see-through pass's first six vertices must be the plate — \
             `Font.PreparedTextBuilder.visit` emits the background before any \
             glyph: got {:?}",
            &see_through[..6].iter().map(|v| v.color).collect::<Vec<_>>()
        );
        assert!(
            !see_through[6..].iter().any(|v| v.color == want),
            "exactly one plate quad, not one per glyph"
        );
        // Both passes carry the same glyph set, so the see-through pass is
        // longer by exactly one quad and no more.
        assert_eq!(
            see_through.len(),
            normal.len() + 6,
            "the see-through pass must be the normal pass's glyphs plus one \
             plate quad"
        );
    }

    /// The plate's rect, predicted from
    /// `Font.PreparedTextBuilder.markBackground` rather than eyeballed, with
    /// the plausible wrong hypothesis evaluated at the same input: a plate
    /// sized to the ink's own bounds (`0 .. total_width` by `0 ..
    /// LINE_HEIGHT`) instead of vanilla's asymmetric one-pixel lead-in on the
    /// left and top only. The two differ by 1 px on three edges, which no
    /// "a plate exists" assertion can separate.
    #[test]
    fn the_plate_rect_is_vanillas_asymmetric_one_not_the_ink_bounds() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);
        let ink = StyledInkLayoutCache::default();

        let text = Text::from_legacy("AB");
        let (_, total_width) = layout_styled_ink_runs(&raster, &text.to_spans());
        let draw = named_pig(text, true);
        let anchor = draw.feet
            + Vec3::new(
                0.0,
                entity_base_height(&draw.type_path) * draw.scale + ATTACHMENT_PADDING,
                0.0,
            );
        let half_width = total_width / 2.0;

        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(
            &raster,
            &ink,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
            &mut normal,
            &mut see_through,
        );

        let corners: Vec<(f32, f32)> = see_through[..6]
            .iter()
            .map(|v| to_local(v, anchor, half_width))
            .collect();
        let min_x = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min);
        let max_x = corners.iter().map(|c| c.0).fold(f32::MIN, f32::max);
        let min_y = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min);
        let max_y = corners.iter().map(|c| c.1).fold(f32::MIN, f32::max);

        // Collected, not asserted one at a time: an `assert!` per edge stops
        // at the first failure and the remaining three edges stay arguments
        // rather than observations.
        let want = [
            ("left", min_x, -BACKGROUND_PAD, 0.0),
            ("right", max_x, total_width, total_width),
            ("top", min_y, -BACKGROUND_PAD, 0.0),
            ("bottom", max_y, metrics::LINE_HEIGHT, metrics::LINE_HEIGHT),
        ];
        let mismatches: Vec<String> = want
            .iter()
            .filter(|(_, got, vanilla, _)| (got - vanilla).abs() > 1e-3)
            .map(|(edge, got, vanilla, ink_bounds)| {
                format!("{edge}: got {got}, vanilla {vanilla}, ink-bounds hypothesis {ink_bounds}")
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "plate rect edges must be `markBackground`'s: {}",
            mismatches.join("; ")
        );
        // The two hypotheses genuinely differ at this input — otherwise the
        // assertion above would pass under either.
        assert!(
            (want[0].2 - want[0].3).abs() > 0.5 && (want[2].2 - want[2].3).abs() > 0.5,
            "the ink-bounds hypothesis must differ from vanilla's on the left \
             and top edges, or this gate measures nothing"
        );
    }

    /// The discrete (sneaking) branch: vanilla makes **one** submission, to
    /// the depth-tested group, carrying the plate. The see-through pass must
    /// stay empty — the pre-existing rule this file already gates — and the
    /// plate must have moved rather than vanished, which is the half a
    /// "see-through pass is empty" assertion cannot see.
    #[test]
    fn a_sneaking_nametag_carries_its_plate_in_the_normal_pass() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);
        let ink = StyledInkLayoutCache::default();

        let draw = named_pig(Text::from_legacy("AB"), false);
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(
            &raster,
            &ink,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
            &mut normal,
            &mut see_through,
        );

        let want = plate_color();
        assert!(
            see_through.is_empty(),
            "sneaking suppresses the see-through pass entirely"
        );
        assert!(
            normal.len() >= 6 && normal[..6].iter().all(|v| v.color == want),
            "a sneaking tag's plate must lead the normal pass: got {:?}",
            normal.iter().take(6).map(|v| v.color).collect::<Vec<_>>()
        );
        assert!(
            !normal[6..].iter().any(|v| v.color == want),
            "exactly one plate quad"
        );
    }

    /// An empty name emits no plate either — the plate is sized from the
    /// line's advance, so a zero-width one would still be a visible
    /// two-pixel smudge floating over the entity if the early return moved
    /// below it.
    #[test]
    fn an_empty_name_emits_no_plate() {
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = scaled_raster("AB", 32, 20, &rgba);
        let ink = StyledInkLayoutCache::default();

        let draw = named_pig(Text::default(), true);
        let mut normal = Vec::new();
        let mut see_through = Vec::new();
        push_entity_quads(
            &raster,
            &ink,
            &draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            &super::super::EntityLightSource::default(),
            &mut normal,
            &mut see_through,
        );
        assert!(
            normal.is_empty() && see_through.is_empty(),
            "an empty name must contribute nothing at all, plate included: {} + {}",
            normal.len(),
            see_through.len()
        );
    }

    /// An [`super::EntityLightSource`] that answers `packed` everywhere.
    fn uniform_light(packed: u8) -> super::super::EntityLightSource {
        super::super::EntityLightSource(Some(Box::new(move |_| Some(packed))))
    }

    /// Draws `draw` under `source` and returns `(normal, see_through)`.
    fn tag_at_light(
        raster: &RasterFont,
        draw: &EntityDraw,
        source: &super::super::EntityLightSource,
    ) -> (Vec<NameTagVertex>, Vec<NameTagVertex>) {
        let (mut normal, mut see_through) = (Vec::new(), Vec::new());
        push_entity_quads(
            raster,
            &StyledInkLayoutCache::default(),
            draw,
            Vec3::ZERO,
            Vec3::X,
            Vec3::Y,
            WorldTextLight::overworld_noon(),
            source,
            &mut normal,
            &mut see_through,
        );
        (normal, see_through)
    }

    /// The two groups take **different** light, and only one of them takes any.
    ///
    /// `text.vsh`'s `IS_SEE_THROUGH` variant is `vertexColor = Color` and
    /// declares no `UV2` input, so vanilla's see-through name tag is
    /// full-bright by construction whatever `lightCoords` it was submitted
    /// with; the depth-tested one is `Color * sample_lightmap(...)` at
    /// `lightCoordsWithEmission(lightCoords, 2)`.
    ///
    /// Three claims, each with the wrong hypothesis named:
    ///
    /// * the see-through range must be **byte-identical** in a pitch-dark cell
    ///   and a bright one — dimming it would be a faithful reading of the
    ///   submission's light argument and a wrong reading of the shader it
    ///   selects;
    /// * the depth-tested range must move — not moving is the pre-fix
    ///   behaviour;
    /// * and it must land on the **floored** byte `0x22`, not on the raw
    ///   `0x00`. Those two differ by a real margin, asserted before the
    ///   comparison so the gate cannot pass by their coinciding.
    #[test]
    fn a_name_tag_dims_only_in_its_depth_tested_group_and_floors_at_emission_two() {
        let Some(raster) = load_font() else {
            return;
        };
        let ambient = lodestone_render::light::OVERWORLD_AMBIENT_LIGHT;
        let draw = named_pig(Text::literal("Babe"), true);

        let dark = uniform_light(0x00);
        let bright = uniform_light(TEXT_FULL_BRIGHT);
        let (dark_normal, dark_see_through) = tag_at_light(&raster, &draw, &dark);
        let (bright_normal, bright_see_through) = tag_at_light(&raster, &draw, &bright);
        assert!(
            !dark_normal.is_empty() && !dark_see_through.is_empty(),
            "a non-discrete tag must contribute to both groups for this gate \
             to compare them"
        );

        let as_pairs = |v: &[NameTagVertex]| {
            v.iter().map(|x| (x.position, x.color)).collect::<Vec<_>>()
        };
        assert_eq!(
            as_pairs(&dark_see_through),
            as_pairs(&bright_see_through),
            "the see-through group samples no lightmap in vanilla, so a \
             pitch-dark cell and a full-bright one must produce identical \
             vertices — plate included"
        );

        let floored = light_coords_with_emission(0x00, 2);
        assert_eq!(floored, 0x22, "the emission floor must apply to both nibbles");
        let want = lodestone_render::light_color(floored, 1.0, ambient);
        let unfloored = lodestone_render::light_color(0x00, 1.0, ambient);
        assert!(
            want[0] - unfloored[0] > 0.02,
            "the floored and unfloored tints must differ measurably, or this \
             gate cannot tell `lightCoordsWithEmission` from a plain sample: \
             {want:?} vs {unfloored:?}"
        );

        let bad: Vec<_> = dark_normal
            .iter()
            .zip(&bright_normal)
            .enumerate()
            .filter(|(_, (d, b))| {
                (0..3).any(|c| (d.color[c] - b.color[c] * want[c]).abs() > 1e-4)
            })
            .map(|(i, (d, b))| (i, d.color, b.color))
            .collect();
        assert!(
            bad.is_empty(),
            "the depth-tested group in a dark cell must be its full-bright \
             self scaled by {want:?}: {} of {} vertices wrong, e.g. (index, \
             got, full-bright source) {:?}",
            bad.len(),
            dark_normal.len(),
            bad.first()
        );
        assert!(
            dark_normal
                .iter()
                .zip(&bright_normal)
                .any(|(d, b)| (d.color[0] - b.color[0]).abs() > 0.05),
            "the depth-tested group must actually have moved — identical \
             vertices in a dark and a bright cell is the pre-fix behaviour"
        );
    }

    /// A **sneaking** tag has only the depth-tested group, and takes the raw
    /// sampled light rather than the emission-floored byte — the second thing
    /// `submitNameTag`'s two branches differ in, after the colour.
    #[test]
    fn a_sneaking_name_tag_takes_the_raw_sample_not_the_emission_floor() {
        let Some(raster) = load_font() else {
            return;
        };
        let ambient = lodestone_render::light::OVERWORLD_AMBIENT_LIGHT;
        let draw = named_pig(Text::literal("Babe"), false);

        let (dark_normal, dark_see_through) = tag_at_light(&raster, &draw, &uniform_light(0x00));
        let (bright_normal, _) = tag_at_light(&raster, &draw, &uniform_light(TEXT_FULL_BRIGHT));
        assert!(!dark_normal.is_empty(), "a sneaking tag must draw ink");
        assert!(
            dark_see_through.is_empty(),
            "a sneaking tag contributes nothing to the see-through group"
        );

        let raw = lodestone_render::light_color(0x00, 1.0, ambient);
        let floored = lodestone_render::light_color(light_coords_with_emission(0x00, 2), 1.0, ambient);
        assert!(
            floored[0] - raw[0] > 0.02,
            "the two hypotheses must differ, or this gate measures nothing: \
             {raw:?} vs {floored:?}"
        );

        let bad: Vec<_> = dark_normal
            .iter()
            .zip(&bright_normal)
            .enumerate()
            .filter(|(_, (d, b))| (0..3).any(|c| (d.color[c] - b.color[c] * raw[c]).abs() > 1e-4))
            .map(|(i, (d, _))| (i, d.color))
            .collect();
        assert!(
            bad.is_empty(),
            "a sneaking tag must take the raw sample {raw:?}, not the \
             emission-floored {floored:?}: {} of {} vertices wrong, e.g. {:?}",
            bad.len(),
            dark_normal.len(),
            bad.first()
        );
    }
}
