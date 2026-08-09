//! Vanilla text for the HUD: proportional advances, real `ascii.png` glyphs and
//! the 1 px drop shadow.
//!
//! # What this replaces
//!
//! [`super::font`] is a hand-drawn 5×7 bitmap with a **fixed 6 px advance**. It
//! is legible, but it is not Minecraft: vanilla's `default` font is
//! proportional, and that is most of the visual difference. `i` is 2 px wide,
//! `l` is 3, `I` and `t` are 4, `W` and `M` are 6 — a fixed-advance render of
//! the same characters is both wider and visibly gappy.
//!
//! That defect is invisible to text-content assertions. `assert_eq!` on the
//! source string passes with every glyph at the wrong width, which is precisely
//! why this module is gated on **pixels** (`tests/vanilla_font_pixels.rs`) and
//! on measured widths rather than on what was drawn.
//!
//! # Where the metrics come from
//!
//! Nothing here invents a width. [`lodestone_assets::font`] parses vanilla's own
//! `font/default.json` provider chain and derives each glyph's advance from the
//! **rightmost non-transparent column of its sheet cell**, exactly as
//! `BitmapProvider.getActualGlyphWidth` does; `RasterFont` adds the decoded
//! sheets so the same cells can be drawn as well as measured. The pen advances
//! by that number, the shadow is `+1` logical px at 25 % of the text colour
//! (`ARGB.scaleRGB(color, 0.25F)` in `Font.PreparedTextBuilder.getShadowColor`),
//! and a glyph's box sits `7 - ascent` logical px below the line's top
//! (`GlyphBitmap.getTop`).
//!
//! # How it draws, and why not a texture
//!
//! Glyph coverage is emitted as **quads on the HUD's existing colour stream**,
//! run-length merged along each row. There is no font atlas upload and no fifth
//! bind group: the HUD's colour pipeline already exists, takes no bind groups at
//! all, and text is the one HUD element whose colour is per-draw rather than
//! per-texel. A textured path would be fewer vertices but would need a new
//! pipeline, a new upload and a new attach point in `app.rs`; this reaches
//! pixels today through calls the shell already makes.
//!
//! # How to change it
//!
//! * **Font is jar-sourced and optional.** [`VanillaFont::shared`] loads once per
//!   process from the same `client.jar` the block/GUI/item atlases come from, and
//!   returns `None` on a jar-less run — where every caller falls back to
//!   [`super::font`] unchanged. Do not make any of this a hard requirement: the
//!   headless and demo paths have no jar, and `hud/item_icon.rs`'s pixel gates
//!   assert against the fixed-width fallback.
//! * **Bold, italic, underline, strikethrough and obfuscated *are* drawn**, in
//!   [`draw_legacy`](VanillaFont::draw_legacy) — see [`legacy_run`](VanillaFont::legacy_run)
//!   and [`glyph_styled`](VanillaFont::glyph_styled). This used to be the one
//!   documented gap in this module (issue #117): the metrics existed
//!   (`Font::advance_bold`, `metrics::ITALIC_SHEAR`) and `§l`/`§o`/`§n`/`§m`/`§k`
//!   were parsed for **width** (`Font::legacy_width`), but the draw side treated
//!   every flag as zero-width state and dropped it, so a bold or italic name
//!   measured correctly and drew as plain text. [`run`](VanillaFont::run) (the
//!   unstyled path `draw`/`draw_plain` use) is untouched: a plain `&str` carries
//!   no `§` codes, so it can never be styled, and giving it the styled glyph path
//!   anyway would cost every title/subtitle/XP-number draw a pool lookup for
//!   nothing.
//! * **Only bitmap providers rasterise.** `unihex` (CJK) and `ttf` parse but
//!   contribute no glyphs, so those codepoints render as the missing-glyph box.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use lodestone_assets::font::{
    FontLoader, FontOptions, GlyphRaster, MISSING_ADVANCE, RasterFont, metrics as font_metrics,
};
use lodestone_assets::{ResourceLocation, ResourceManager};
use lodestone_model::text::{TextColor, TextSpan};

use super::item_icon::ColourStream;

/// A [`TextColor`] as the sRGB 0..1 triple the HUD's colour stream takes.
///
/// [`TextColor::rgb`] is the single source of truth for the sixteen named
/// values (transcribed there from vanilla's `TextColor.java`); this only
/// unpacks it. The division by 255 with **no** transfer function is deliberate
/// and is the whole colour-management story for text: vanilla is not
/// colour-managed, so its `0xAA` is written to the framebuffer as the sRGB byte
/// `0xAA`, and the shadow's quarter (`shadow_of`) is taken on these same gamma
/// values. Converting to linear here would lighten every named colour and lift
/// the shadow from vanilla's near-black to a visible grey.
#[must_use]
pub fn text_color_rgb(color: TextColor) -> [f32; 3] {
    let hex = color.rgb();
    [
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ]
}

/// One glyph run's active formatting, tracked across a `§`-coded string.
///
/// A legacy colour code or `§r` resets every one of these to `false`
/// (`apply_legacy_code` in `lodestone-model/src/text.rs:626-644`: "a legacy
/// colour code resets all formatting to just that colour"); this type carries
/// no colour of its own for that reason — [`legacy_run`](VanillaFont::legacy_run)
/// tracks colour separately and clears both together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GlyphStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    obfuscated: bool,
}

impl GlyphStyle {
    /// Whether any effect line (`§n`/`§m`) is active — the only flags that
    /// need geometry *beyond* the glyph quads themselves.
    fn has_effect(self) -> bool {
        self.underline || self.strikethrough
    }
}

/// Vanilla's missing-glyph box: a 5×8 hollow rectangle with a 1 px edge, advance
/// `5 + 1`. Mirrors `SpecialGlyphs.MISSING` in the 26.2 client.
const MISSING_W: u32 = 5;
/// Height of the missing-glyph box, in logical pixels.
const MISSING_H: u32 = 8;

/// The vanilla `minecraft:default` font, loaded with pixels, ready to draw.
#[derive(Debug)]
pub struct VanillaFont {
    raster: RasterFont,
    /// `§k` obfuscated text's replacement pool: drawable codepoints grouped by
    /// `ceil(advance)`, mirroring `FontSet.glyphsByWidth`
    /// (`FontSet.java:58,109,160-163`). Vanilla's own pool is built from *every*
    /// active provider (including `space`), but only bitmap glyphs are
    /// drawable here, so this is restricted to codepoints
    /// [`RasterFont::raster`] actually returns pixels for — a codepoint with no
    /// ink never gets picked as a replacement, which is the one place this
    /// diverges from vanilla's pool (documented, not a bug: it would otherwise
    /// occasionally "obfuscate" a glyph into invisible whitespace).
    obfuscation_pool: HashMap<u32, Vec<char>>,
    /// Free-running state for the obfuscated-glyph picker. Vanilla never
    /// reseeds `Font.random` (`Font.java:34`, `RandomSource.create()` at
    /// construction) — every glyph drawn advances the same stream, so the
    /// same text resamples differently **every frame** with no timer
    /// involved, which is what makes `§k` read as animated rather than a
    /// one-shot randomised label. `AtomicU64` reproduces that: `&self` is
    /// shared (this font lives behind an `Arc`), so the state has to be
    /// interior-mutable, and every glyph draw advances it exactly once, same
    /// as vanilla.
    obfuscation_rng: AtomicU64,
}

/// Process-wide cache. The jar is ~37 MB and the font is tiny; loading it once
/// keeps `HudRenderer::new` cheap even when a test builds several renderers.
static SHARED: OnceLock<Option<Arc<VanillaFont>>> = OnceLock::new();

impl VanillaFont {
    /// The process-wide vanilla font, loaded on first call from the same pack
    /// the other vanilla atlases come from.
    ///
    /// Fail-open by design: a jar-less run (headless gates, the demo world)
    /// gets `None` and every caller keeps the fixed-width fallback, which is
    /// what makes this module safe to wire in unconditionally.
    #[must_use]
    pub fn shared() -> Option<Arc<VanillaFont>> {
        SHARED.get_or_init(Self::load).clone()
    }

    fn load() -> Option<Arc<VanillaFont>> {
        let manager = jar_manager()?;
        let id: ResourceLocation = "minecraft:default".parse().ok()?;
        match FontLoader::new(&manager).load_raster(&id, &FontOptions::none()) {
            Ok(raster) => {
                tracing::info!(
                    target: "assets",
                    codepoints = raster.font().codepoint_count(),
                    sheets = raster.sheet_count(),
                    "loaded the vanilla default font for the HUD"
                );
                let obfuscation_pool = build_obfuscation_pool(&raster);
                Some(Arc::new(VanillaFont {
                    raster,
                    obfuscation_pool,
                    obfuscation_rng: AtomicU64::new(0x9E37_79B9_7F4A_7C15),
                }))
            }
            Err(e) => {
                tracing::warn!(target: "assets", "load vanilla font: {e}");
                None
            }
        }
    }

    /// Builds directly from an already-open pack — the way to point the HUD at a
    /// resource pack, or at a pack a test pinned, rather than at discovery's
    /// choice.
    ///
    /// **Currently has no callers.** `tests/vanilla_font_pixels.rs` goes through
    /// [`HudRenderer::new`](crate::hud::HudRenderer::new) +
    /// [`font_attached`](crate::hud::HudRenderer::font_attached) instead,
    /// deliberately: that exercises the path the shipped client actually takes,
    /// including discovery. Feed the result to
    /// [`HudRenderer::attach_font`](crate::hud::HudRenderer::attach_font).
    pub fn from_manager(manager: &ResourceManager) -> Result<Self, lodestone_assets::FontError> {
        let id: ResourceLocation = "minecraft:default"
            .parse()
            .expect("minecraft:default is a valid location");
        let raster = FontLoader::new(manager).load_raster(&id, &FontOptions::none())?;
        let obfuscation_pool = build_obfuscation_pool(&raster);
        Ok(VanillaFont {
            raster,
            obfuscation_pool,
            obfuscation_rng: AtomicU64::new(0x9E37_79B9_7F4A_7C15),
        })
    }

    /// The advance of `ch` in **device** pixels at `scale`.
    #[must_use]
    pub fn advance(&self, ch: char, scale: f32) -> f32 {
        self.raster
            .advance(ch as u32)
            .unwrap_or(MISSING_ADVANCE)
            .mul_add(scale, 0.0)
    }

    /// The width of a plain string in device pixels at `scale`.
    #[must_use]
    pub fn width(&self, s: &str, scale: f32) -> f32 {
        self.raster.string_width(s) * scale
    }

    /// The width of a `§`-coded string in device pixels at `scale`. `§`+code
    /// pairs are zero-width, matching both vanilla and the old fixed path.
    #[must_use]
    pub fn legacy_width(&self, s: &str, scale: f32) -> f32 {
        self.raster.legacy_width(s) * scale
    }

    /// The width of a styled span list in device pixels at `scale`.
    ///
    /// This is the measurement half of [`draw_spans`](Self::draw_spans), and it
    /// takes the route `Font::legacy_width`'s own doc prescribes for structured
    /// components: decompose to `(codepoint, bold)` and call
    /// [`advance_bold`](lodestone_assets::font::Font::advance_bold). Bold is the
    /// only flag that changes an advance (`+1` per glyph); italic shears in
    /// place, and underline/strikethrough/obfuscated leave the pen alone.
    #[must_use]
    pub fn spans_width(&self, spans: &[TextSpan], scale: f32) -> f32 {
        let font = self.raster.font();
        let total: f32 = spans
            .iter()
            .map(|span| {
                let bold = span.style.bold.unwrap_or(false);
                span.text
                    .chars()
                    .map(|ch| {
                        font.advance_bold(ch as u32, bold)
                            .unwrap_or(MISSING_ADVANCE + if bold { 1.0 } else { 0.0 })
                    })
                    .sum::<f32>()
            })
            .sum();
        total * scale
    }

    /// Draw `s` with its vanilla drop shadow, the string's top-left at `(x, y)`.
    ///
    /// Two passes: the shadow copy first, offset `+1` logical pixel on **both**
    /// axes at 25 % of the colour, then the text. Drawing the whole string's
    /// shadow before any of its glyphs is what keeps a following glyph's ink on
    /// top of the previous glyph's shadow, which is what vanilla's two-layer
    /// batch does.
    pub(crate) fn draw(
        &self,
        cs: &mut ColourStream<'_>,
        s: &str,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
    ) {
        let off = font_metrics::SHADOW_OFFSET * scale;
        self.run(cs, s, x + off, y + off, scale, shadow_of(c));
        self.run(cs, s, x, y, scale, c);
    }

    /// Draw a `§`-coded string with its drop shadow. Colour codes recolour the
    /// following run, `§r` resets to `base`, and format codes draw real
    /// geometry — see [`legacy_run`](Self::legacy_run).
    pub(crate) fn draw_legacy(
        &self,
        cs: &mut ColourStream<'_>,
        s: &str,
        x: f32,
        y: f32,
        scale: f32,
        base: [f32; 3],
        alpha: f32,
    ) {
        let off = font_metrics::SHADOW_OFFSET * scale;
        self.legacy_run(cs, s, x + off, y + off, scale, base, alpha, true);
        self.legacy_run(cs, s, x, y, scale, base, alpha, false);
    }

    /// Draw a list of styled spans with its drop shadow.
    ///
    /// This is the **structured** counterpart to
    /// [`draw_legacy`](Self::draw_legacy), and the difference is not stylistic.
    /// `draw_legacy`'s input vocabulary is `§` codes in a `&str`, which can
    /// express only the sixteen named colours — a server's
    /// [`TextColor::Rgb`] has no legacy code, so routing a modern component
    /// through a legacy string silently discards its colour before this module
    /// ever sees it. Spans carry [`TextColor`] itself, so hex survives.
    ///
    /// A span whose `style.color` is `None` draws in `base`. That is
    /// [`TextStyle::inherit`](lodestone_model::text::TextStyle::inherit)'s
    /// contract reaching its terminus: `to_spans` has already resolved
    /// inheritance down the tree, so a `None` here means the colour was
    /// unspecified all the way to the root and the *surface* decides — white for
    /// the sidebar title, grey for a server's MOTD.
    pub(crate) fn draw_spans(
        &self,
        cs: &mut ColourStream<'_>,
        spans: &[TextSpan],
        x: f32,
        y: f32,
        scale: f32,
        base: [f32; 3],
        alpha: f32,
    ) {
        let off = font_metrics::SHADOW_OFFSET * scale;
        self.spans_run(cs, spans, x + off, y + off, scale, base, alpha, true);
        self.spans_run(cs, spans, x, y, scale, base, alpha, false);
    }

    /// One unshadowed pass over a styled span list. Mirrors
    /// [`legacy_run`](Self::legacy_run) exactly — same `glyph_styled` primitive,
    /// same `shadow` colour scaling so both passes walk identical geometry, same
    /// `position == 0` rule for where an effect bar's left edge starts — and
    /// differs only in where the style comes from. `position` counts glyphs
    /// across the whole span *list*, not per span, because vanilla's
    /// `position == 0` check (`Font.java:274`) is about the first glyph of the
    /// rendered line and a span boundary is not a line boundary.
    #[allow(clippy::too_many_arguments)]
    fn spans_run(
        &self,
        cs: &mut ColourStream<'_>,
        spans: &[TextSpan],
        x: f32,
        y: f32,
        scale: f32,
        base: [f32; 3],
        alpha: f32,
        shadow: bool,
    ) {
        let mut cursor = x;
        let mut position = 0usize;
        for span in spans {
            let style = GlyphStyle {
                bold: span.style.bold.unwrap_or(false),
                italic: span.style.italic.unwrap_or(false),
                underline: span.style.underlined.unwrap_or(false),
                strikethrough: span.style.strikethrough.unwrap_or(false),
                obfuscated: span.style.obfuscated.unwrap_or(false),
            };
            let rgb = span.style.color.map_or(base, text_color_rgb);
            let c = [rgb[0], rgb[1], rgb[2], alpha];
            let c = if shadow { shadow_of(c) } else { c };
            for ch in span.text.chars() {
                cursor += self.glyph_styled(cs, ch, cursor, y, scale, c, style, position == 0);
                position += 1;
            }
        }
    }

    /// Draw `s` with **no** drop shadow, the string's top-left at `(x, y)`.
    ///
    /// Vanilla's `graphics.text(font, component, x, y, colour, shadow)` takes the
    /// flag as an argument, and the two container labels
    /// (`AbstractContainerScreen.java:190-191`) pass `false`. Every other text
    /// surface in this crate passes it implicitly by calling
    /// [`draw`](Self::draw), so the shadowless case needs its own name rather
    /// than a bool parameter on the common path.
    pub(crate) fn draw_plain(
        &self,
        cs: &mut ColourStream<'_>,
        s: &str,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
    ) {
        self.run(cs, s, x, y, scale, c);
    }

    /// One unshadowed pass over a plain string.
    fn run(&self, cs: &mut ColourStream<'_>, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let mut cursor = x;
        for ch in s.chars() {
            cursor += self.glyph(cs, ch, cursor, y, scale, c);
        }
    }

    /// One unshadowed pass over a `§`-coded string. `shadow` scales every run's
    /// colour, so the two passes walk identical geometry. Format codes now
    /// carry real geometry (issue #117): `style` tracks the five flags across
    /// the run exactly as `Font::legacy_width` already tracks bold for
    /// measurement, with the same reset rule
    /// (`apply_legacy_code`, `lodestone-model/src/text.rs:626-644`) — a colour
    /// code or `§r` clears every flag, not just the one it names.
    #[allow(clippy::too_many_arguments)]
    fn legacy_run(
        &self,
        cs: &mut ColourStream<'_>,
        s: &str,
        x: f32,
        y: f32,
        scale: f32,
        base: [f32; 3],
        alpha: f32,
        shadow: bool,
    ) {
        let mut cursor = x;
        let mut rgb = base;
        let mut style = GlyphStyle::default();
        let mut position = 0usize;
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                match chars.next() {
                    Some(code) => {
                        if let Some(v) = super::legacy_rgb(code) {
                            rgb = v;
                            style = GlyphStyle::default();
                        } else if code.eq_ignore_ascii_case(&'r') {
                            rgb = base;
                            style = GlyphStyle::default();
                        } else {
                            match code.to_ascii_lowercase() {
                                'l' => style.bold = true,
                                'o' => style.italic = true,
                                'n' => style.underline = true,
                                'm' => style.strikethrough = true,
                                'k' => style.obfuscated = true,
                                _ => {}
                            }
                        }
                    }
                    None => break,
                }
                continue;
            }
            let c = [rgb[0], rgb[1], rgb[2], alpha];
            let c = if shadow { shadow_of(c) } else { c };
            cursor += self.glyph_styled(cs, ch, cursor, y, scale, c, style, position == 0);
            position += 1;
        }
    }

    /// Draw one glyph with the line's top-left at `(x, y)` and return its
    /// advance in device pixels. No style: this is [`run`](Self::run)'s glyph
    /// primitive, and `run` draws plain `&str`s that can never carry a `§`
    /// code — see [`glyph_styled`](Self::glyph_styled) for the `legacy_run`
    /// counterpart.
    fn glyph(
        &self,
        cs: &mut ColourStream<'_>,
        ch: char,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
    ) -> f32 {
        let cp = ch as u32;
        let Some(r) = self.raster.raster(cp) else {
            // Covered but not drawable = whitespace (the `space` provider);
            // uncovered = vanilla's hollow missing-glyph box.
            if self.raster.font().contains(cp) {
                return self.raster.advance(cp).unwrap_or(MISSING_ADVANCE) * scale;
            }
            missing_box(cs, x, y, scale, c);
            return MISSING_ADVANCE * scale;
        };
        self.draw_ink(cs, &r, x, y, scale, c, false);
        r.advance() * scale
    }

    /// Draw one glyph honouring `style`, with the line's top-left at
    /// `(x, y)`. `first` is whether this is the very first glyph of the
    /// **run** (not the string overall — `legacy_run` restarts its own
    /// counter each pass), matching `Font.java:274`'s `position == 0` check
    /// for where the underline/strikethrough bar's left edge starts.
    ///
    /// Returns the advance in device pixels, computed from `ch`'s **own**
    /// glyph — even when `style.obfuscated` swaps in a different codepoint's
    /// pixels, see [`obfuscation_pool`](VanillaFont::obfuscation_pool)'s field
    /// docs for why. Effects (underline/strikethrough) and the background
    /// advance vanilla marks per glyph (`Font.java:284`, `markBackground`) are
    /// emitted here **unconditionally** — including for whitespace and the
    /// missing-glyph box — because vanilla's own `accept()` runs the same way
    /// for every character the string decomposes to, ink or not: an
    /// underlined phrase's line does not gap at its spaces.
    #[allow(clippy::too_many_arguments)]
    fn glyph_styled(
        &self,
        cs: &mut ColourStream<'_>,
        ch: char,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
        style: GlyphStyle,
        first: bool,
    ) -> f32 {
        let cp = ch as u32;
        let base_r = self.raster.raster(cp);
        let advance = match base_r {
            Some(r) => r.advance(),
            None if self.raster.font().contains(cp) => {
                self.raster.advance(cp).unwrap_or(MISSING_ADVANCE)
            }
            None => {
                missing_box(cs, x, y, scale, c);
                MISSING_ADVANCE
            }
        };
        // `GlyphInfo::getAdvance(bold)` (`GlyphInfo.java:6-8`): `advance +
        // boldOffset` when bold, unchanged otherwise. Vanilla applies this to
        // *every* glyph, drawable or not — a bold space is 1px wider too.
        let bold_extra = if style.bold {
            font_metrics::BOLD_OFFSET
        } else {
            0.0
        };
        let bold_advance = advance + bold_extra;

        if let Some(base_r) = base_r {
            // `§k` (and not a space, per `Font.getGlyph`'s `codepoint != 32`
            // guard, which space satisfies here by having no raster at all):
            // substitute a same-width-class glyph's pixels, but keep drawing
            // at `ch`'s own metrics.
            let draw_r = if style.obfuscated {
                self.obfuscated_raster(advance).unwrap_or(base_r)
            } else {
                base_r
            };
            self.draw_ink(cs, &draw_r, x, y, scale, c, style.italic);
            if style.bold {
                // The second, offset pass that actually makes bold read as
                // bold (`BakedSheetGlyph.renderChar`, `BakedSheetGlyph.java:110-113`)
                // — not a font-weight variant, the same glyph redrawn shifted.
                self.draw_ink(
                    cs,
                    &draw_r,
                    x + bold_extra * scale,
                    y,
                    scale,
                    c,
                    style.italic,
                );
            }
        }

        if style.has_effect() {
            // `Font.java:274`: `effectX0 = position == 0 ? x - 1.0F : x`.
            let x0 = if first {
                x - font_metrics::EFFECT_LEAD_IN * scale
            } else {
                x
            };
            let x1 = x + bold_advance * scale;
            let thickness = font_metrics::EFFECT_THICKNESS * scale;
            if style.strikethrough {
                // `Font.java:285-291`: bar bottom at `y + 4.5F`.
                let bottom = y + font_metrics::STRIKETHROUGH_Y * scale;
                cs.rect(x0, bottom - thickness, x1 - x0, thickness, c);
            }
            if style.underline {
                // `Font.java:293-299`: bar bottom at `y + 9.0F`.
                let bottom = y + font_metrics::UNDERLINE_Y * scale;
                cs.rect(x0, bottom - thickness, x1 - x0, thickness, c);
            }
        }

        bold_advance * scale
    }

    /// Emit one glyph raster's ink as merged horizontal runs, with the line's
    /// top-left at `(x, y)`. An 8×8 cell is at most 8 quads instead of up to
    /// 64, and the merged quad is pixel-identical because every texel in a
    /// run shares one colour.
    ///
    /// When `italic`, each row is sheared independently: `v` is that row's own
    /// logical-pixel offset from the line's top (matching what
    /// [`GlyphRaster::top`] returns for the glyph's top edge), and the row
    /// shifts in `x` by `ITALIC_SHEAR - ITALIC_SHEAR_SLOPE * v`
    /// (`BakedSheetGlyph.shearTop`/`shearBottom`,
    /// `BakedSheetGlyph.java:144-150`, both `1.0F - 0.25F * v`). Vanilla shears
    /// the whole glyph as one quad with two sheared edges (a continuous linear
    /// interpolation between the top and bottom edge's shear); this evaluates
    /// that same affine function per texel row instead, which is the run-based
    /// renderer's equivalent of "per scanline" once nearest-neighbour sampling
    /// is accounted for — texel rows already are the sampling granularity here.
    fn draw_ink(
        &self,
        cs: &mut ColourStream<'_>,
        r: &GlyphRaster<'_>,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
        italic: bool,
    ) {
        let texel = r.texel_size() * scale;
        let top = y + r.top() * scale;
        for ty in 0..r.cell_height() {
            let mut tx = 0;
            while tx < r.cell_width() {
                if !r.is_ink(tx, ty) {
                    tx += 1;
                    continue;
                }
                let start = tx;
                while tx < r.cell_width() && r.is_ink(tx, ty) {
                    tx += 1;
                }
                let shear = if italic {
                    let v = r.top() + (ty as f32 + 0.5) * r.texel_size();
                    (font_metrics::ITALIC_SHEAR - font_metrics::ITALIC_SHEAR_SLOPE * v) * scale
                } else {
                    0.0
                };
                cs.rect(
                    x + shear + start as f32 * texel,
                    top + ty as f32 * texel,
                    (tx - start) as f32 * texel,
                    texel,
                    c,
                );
            }
        }
    }

    /// Picks a `§k` replacement raster from [`obfuscation_pool`](VanillaFont::obfuscation_pool),
    /// keyed by `ceil(original_advance)` — vanilla's own width class
    /// (`FontSet.java:109`, `Mth.ceil(glyph.info().getAdvance(false))`), and
    /// advances the free-running picker once. `None` only when this font has
    /// no drawable glyph at all of that exact rounded width.
    fn obfuscated_raster(&self, original_advance: f32) -> Option<GlyphRaster<'_>> {
        let width = original_advance.ceil();
        if !(0.0..4096.0).contains(&width) {
            return None;
        }
        let pool = self.obfuscation_pool.get(&(width as u32))?;
        if pool.is_empty() {
            return None;
        }
        // SplitMix64: cheap, well-mixed, and `Ordering::Relaxed` is enough —
        // this only needs "looks random across frames", not a CSPRNG or any
        // cross-thread ordering guarantee.
        let state = self
            .obfuscation_rng
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
        let mut z = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let idx = (z as usize) % pool.len();
        self.raster.raster(pool[idx] as u32)
    }
}

/// Groups every codepoint this font can actually draw pixels for by
/// `ceil(advance)`, mirroring `FontSet.glyphsByWidth`
/// (`FontSet.java:58,109,160-163`) restricted to codepoints
/// [`RasterFont::raster`] returns coverage for. Built once at load time so
/// `§k` never rebuilds it mid-draw.
fn build_obfuscation_pool(raster: &RasterFont) -> HashMap<u32, Vec<char>> {
    let mut pool: HashMap<u32, Vec<char>> = HashMap::new();
    for cp in raster.font().codepoints() {
        if raster.raster(cp).is_none() {
            continue;
        }
        let Some(ch) = char::from_u32(cp) else {
            continue;
        };
        let Some(advance) = raster.advance(cp) else {
            continue;
        };
        let width = advance.ceil();
        if !(0.0..4096.0).contains(&width) {
            continue;
        }
        pool.entry(width as u32).or_default().push(ch);
    }
    pool
}

/// The one vanilla `client.jar` [`ResourceManager`], from
/// [`crate::resources::vanilla_manager`].
///
/// **This used to be a hand-copied duplicate of that function** — its own pack
/// discovery (`LODESTONE_ASSETS`, else the highest-sorting `.cache/mc/<version>`
/// holding both `client.jar` and `generated/reports/blocks.json`), its own
/// `std::fs::read` of the jar, and a doc comment explaining that the duplication
/// existed only because `resources::vanilla_manager` was `#[cfg(test)]` and asking for
/// that attribute to be dropped. It is no longer `#[cfg(test)]`, and that comment had
/// gone stale: `resources`' own doc now invites exactly this collapse.
///
/// Deleting the copy is what makes the font work in a browser, and it is worth being
/// precise about why, because it is not a tidy-up: the browser's jar arrives as
/// `fetch`ed bytes through `crate::platform::assets`, and `vanilla_manager` is the
/// single place that knows it. A duplicate that reads a path instead would have
/// produced a title screen with a **readable-looking layout and no glyphs** — every
/// caller here falls back to a fixed-width stand-in when this returns `None`, which is
/// exactly the shape of failure that reports success while the screen is wrong.
fn jar_manager() -> Option<ResourceManager> {
    crate::resources::vanilla_manager()
}


/// Vanilla's shadow colour: `ARGB.scaleRGB(color, 0.25F)` — a **gamma-space**
/// quarter of each channel with alpha preserved.
///
/// The HUD's colour convention is sRGB 0..1 written verbatim (see
/// `hud::legacy_rgb`, which divides vanilla's hex codes by 255), so the quarter
/// is taken directly on those floats. Doing it in linear space would lift the
/// shadow to ~54 % on screen — a grey outline instead of vanilla's near-black one.
#[must_use]
pub fn shadow_of(c: [f32; 4]) -> [f32; 4] {
    [
        c[0] * font_metrics::SHADOW_BRIGHTNESS,
        c[1] * font_metrics::SHADOW_BRIGHTNESS,
        c[2] * font_metrics::SHADOW_BRIGHTNESS,
        c[3],
    ]
}

/// Vanilla's missing-glyph box: a 5×8 hollow rectangle, 1 px edge, drawn as four
/// bars so it costs four quads instead of a per-pixel walk.
fn missing_box(cs: &mut ColourStream<'_>, x: f32, y: f32, scale: f32, c: [f32; 4]) {
    let w = MISSING_W as f32 * scale;
    let h = MISSING_H as f32 * scale;
    cs.rect(x, y, w, scale, c);
    cs.rect(x, y + h - scale, w, scale, c);
    cs.rect(x, y, scale, h, c);
    cs.rect(x + w - scale, y, scale, h, c);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shadow is a quarter of each channel in the space the HUD works in,
    /// with alpha untouched. Vanilla's `ARGB.scaleRGB(0xFFFFFFFF, 0.25F)` is
    /// `0xFF3F3F3F` — 63/255 = 0.247, which is what a white text colour must
    /// produce here.
    #[test]
    fn shadow_is_a_gamma_space_quarter_with_alpha_preserved() {
        let s = shadow_of([1.0, 1.0, 1.0, 0.6]);
        assert!((s[0] - 0.25).abs() < 1e-6, "got {s:?}");
        assert!((s[1] - 0.25).abs() < 1e-6, "got {s:?}");
        assert!((s[2] - 0.25).abs() < 1e-6, "got {s:?}");
        assert!(
            (s[3] - 0.6).abs() < 1e-6,
            "alpha must be preserved, not scaled: {s:?}"
        );
        // The 8-bit value vanilla writes, for the avoidance of doubt.
        assert_eq!((s[0] * 255.0) as u8, 63);
    }
}

/// Pixel-geometry gates for issue #117: bold, italic, underline, strikethrough
/// and obfuscated must draw real geometry, not just measure zero-width. Each
/// test predicts an exact value from a formula transcribed from
/// `.cache/mc/26.2/client-src` (quoted per-test), not merely a sign or a
/// "something painted" check — the CLAUDE.md *magnitude* species repair.
///
/// These need the real vanilla font (real `ascii.png` glyph shapes and
/// advances), so they are gated on the jar like every sibling GPU/asset gate
/// in this crate and run with `--ignored`:
///
/// ```text
/// cargo test -p lodestone-shell --lib -- --ignored --nocapture styling_tests
/// ```
///
/// They need no GPU adapter at all — `VanillaFont::draw_legacy` writes
/// straight into a plain `Vec<f32>` `ColourStream`, the same vertex format
/// [`HudGeometry`](crate::hud::HudGeometry) produces — but they are `#[ignore]`d
/// anyway, matching `lodestone-assets/tests/real_jar.rs`'s convention: a
/// missing `client.jar` is a **fail-closed** precondition, not a skip, so the
/// default `cargo test --workspace` run stays hermetic while still requiring
/// `--ignored` runs to prove something rather than silently pass on a jar-less
/// host.
#[cfg(test)]
mod styling_tests {
    use super::*;
    use lodestone_assets::font::metrics as font_metrics;

    /// A real vanilla font, or a loud failure — never a silent skip (see the
    /// module doc).
    fn font() -> VanillaFont {
        let manager = crate::resources::vanilla_manager().expect(
            "styling gate opted in via --ignored but no vanilla client.jar was found; set \
             LODESTONE_ASSETS to a pack root containing client.jar, or populate \
             .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
        );
        VanillaFont::from_manager(&manager).expect("build the vanilla font")
    }

    const W: f32 = 400.0;
    const H: f32 = 200.0;
    const ORIGIN_X: f32 = 50.0;
    const ORIGIN_Y: f32 = 50.0;

    /// Draws `s` (a `§`-coded string) at `(ORIGIN_X, ORIGIN_Y)`, scale 1, and
    /// returns every **main-pass** ink vertex's top-left as `(x, y)` in local
    /// pixel space — the inverse of [`ColourStream::rect`]'s NDC transform.
    /// Filtering to `colour` excludes the shadow pass (`shadow_of` scales
    /// every channel to `SHADOW_BRIGHTNESS`, so it never matches an
    /// unscaled `colour`), which is what lets a single draw call stand in for
    /// "just the ink", matching how `container_labels.rs` isolates label ink
    /// by colour rather than by re-deriving vanilla's two-pass structure here.
    fn ink_points(font: &VanillaFont, s: &str, colour: [f32; 3]) -> Vec<(f32, f32)> {
        let mut verts = Vec::new();
        {
            let mut cs = ColourStream {
                verts: &mut verts,
                w: W,
                h: H,
            };
            font.draw_legacy(&mut cs, s, ORIGIN_X, ORIGIN_Y, 1.0, colour, 1.0);
        }
        verts
            .chunks_exact(6)
            .filter(|v| {
                (v[2] - colour[0]).abs() < 1e-4
                    && (v[3] - colour[1]).abs() < 1e-4
                    && (v[4] - colour[2]).abs() < 1e-4
            })
            .map(|v| ((v[0] + 1.0) * W * 0.5, (1.0 - v[1]) * H * 0.5))
            .collect()
    }

    fn bbox(points: &[(f32, f32)]) -> (f32, f32, f32, f32) {
        let x0 = points.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let x1 = points
            .iter()
            .map(|p| p.0)
            .fold(f32::NEG_INFINITY, f32::max);
        let y0 = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let y1 = points
            .iter()
            .map(|p| p.1)
            .fold(f32::NEG_INFINITY, f32::max);
        (x0, y0, x1, y1)
    }

    fn width_of(points: &[(f32, f32)]) -> f32 {
        let (x0, _, x1, _) = bbox(points);
        x1 - x0
    }

    /// **Bold**: `BakedSheetGlyph.renderChar` (`BakedSheetGlyph.java:110-113`)
    /// redraws the same glyph a second time, offset `+boldOffset` in x. Ink's
    /// bounding box must therefore widen by *exactly* `BOLD_OFFSET` (at
    /// `scale = 1.0`, device px == logical px) — not "wider", the specific
    /// number, per CLAUDE.md's magnitude-species repair.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn bold_ink_is_exactly_bold_offset_wider() {
        let font = font();
        let plain = ink_points(&font, "l", [1.0, 1.0, 1.0]);
        let bold = ink_points(&font, "\u{a7}ll", [1.0, 1.0, 1.0]);
        assert!(!plain.is_empty(), "plain 'l' must draw ink at all");
        assert!(!bold.is_empty(), "bold 'l' must draw ink at all");
        let delta = width_of(&bold) - width_of(&plain);
        assert!(
            (delta - font_metrics::BOLD_OFFSET).abs() < 1e-3,
            "bold must widen ink by exactly BOLD_OFFSET={} device px; measured delta={delta} \
             (plain width={}, bold width={})",
            font_metrics::BOLD_OFFSET,
            width_of(&plain),
            width_of(&bold)
        );
    }

    /// Control for the above: a *plain* 'l' drawn twice must produce
    /// bit-identical ink — proves the width delta above is attributable to
    /// `§l`, not to draw-to-draw noise.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn plain_glyph_draw_is_deterministic() {
        let font = font();
        let a = ink_points(&font, "l", [1.0, 1.0, 1.0]);
        let b = ink_points(&font, "l", [1.0, 1.0, 1.0]);
        assert_eq!(a, b);
    }

    /// **Italic**: each ink row shears in x by
    /// `ITALIC_SHEAR - ITALIC_SHEAR_SLOPE * v`, `v` being that row's own
    /// logical-pixel offset from the line's top
    /// (`BakedSheetGlyph.shearTop`/`shearBottom`, `BakedSheetGlyph.java:144-150`).
    /// This predicts the exact x offset between the topmost and bottommost ink
    /// row of an italic `'|'` (a single-column vertical stroke with **no**
    /// serif — verified directly against `RasterFont::raster('|')`'s ink
    /// mask: column 0 only, rows 0..6, row 7 blank — so each row's ink is
    /// exactly one column and easy to compare) from that formula, fed with
    /// the row positions **measured from the plain draw** — i.e. the expected
    /// value is derived from the glyph's own real geometry, not restated as a
    /// hardcoded row count.
    ///
    /// `'l'` was tried first and rejected: its real ascii-sheet glyph has a
    /// small foot flare on its lowest ink row, so that row's leftmost ink
    /// column is not the same column the rows above it use — a real
    /// difference in the *glyph's shape*, not in the shear, but this test's
    /// "leftmost ink at this row's y" measurement cannot tell the two apart.
    /// `'|'` has no such flare (confirmed above), which is exactly the CLAUDE.md
    /// lesson about a control's premise needing to be checked rather than
    /// assumed — here the premise was "this glyph is a straight column", and
    /// `'l'` quietly wasn't.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn italic_shear_matches_the_bakedsheetglyph_formula() {
        let font = font();
        let plain = ink_points(&font, "|", [1.0, 1.0, 1.0]);
        let italic = ink_points(&font, "\u{a7}o|", [1.0, 1.0, 1.0]);
        assert!(!plain.is_empty());
        assert!(!italic.is_empty());

        let (_, top_y, _, _) = bbox(&plain);
        let (_, _, _, bottom_y) = bbox(&plain);
        // `bottom_y` from `bbox` is the bottom **edge** of the last ink row
        // (top + texel); step back one texel to get that row's own top-left,
        // matching `top_y`'s convention. `'|'`'s stroke is one texel wide in
        // the ascii sheet, so this is exact for an un-italicised glyph.
        let texel = {
            let plain_row = plain
                .iter()
                .filter(|(_, y)| (*y - top_y).abs() < 1e-3)
                .count();
            assert!(plain_row >= 1);
            // Row height in device px: the vertical gap between this glyph's
            // only two distinct row y-values it actually reaches at scale 1
            // is exactly one texel — read directly off the plain draw's own
            // two extreme rows rather than assumed.
            let mut ys: Vec<f32> = plain.iter().map(|p| p.1).collect();
            ys.sort_by(f32::total_cmp);
            ys.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
            assert!(ys.len() >= 2, "need at least two distinct ink rows: {ys:?}");
            ys[1] - ys[0]
        };
        let bottom_row_top = bottom_y - texel;

        let left_at_row = |points: &[(f32, f32)], row_y: f32| -> f32 {
            points
                .iter()
                .filter(|(_, y)| (*y - row_y).abs() < 1e-3)
                .map(|p| p.0)
                .fold(f32::INFINITY, f32::min)
        };
        let measured_top_x = left_at_row(&italic, top_y);
        let measured_bottom_x = left_at_row(&italic, bottom_row_top);

        let v_of = |row_top_y: f32| (row_top_y - ORIGIN_Y) + 0.5 * texel;
        let shear_at =
            |v: f32| font_metrics::ITALIC_SHEAR - font_metrics::ITALIC_SHEAR_SLOPE * v;
        let expected = shear_at(v_of(top_y)) - shear_at(v_of(bottom_row_top));
        let measured = measured_top_x - measured_bottom_x;

        assert!(
            (measured - expected).abs() < 1e-2,
            "italic shear between the top and bottom ink row must equal \
             ITALIC_SHEAR - ITALIC_SHEAR_SLOPE * v evaluated at each row's own centre; \
             expected {expected}, measured {measured} (top_x={measured_top_x}, \
             bottom_x={measured_bottom_x}, texel={texel})"
        );
        // Sign check as a sanity backstop: vanilla's italic leans the *top*
        // to the right relative to the bottom.
        assert!(
            measured > 0.0,
            "the top row must sit to the right of the bottom row for italic text"
        );
    }

    /// Control for the above: a *plain* `'|'` has zero shear — every ink row
    /// starts at the same x. If this fails, the "top row vs bottom row" split
    /// above is measuring some other x-varying effect, not italic.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn plain_glyph_has_no_shear() {
        let font = font();
        let plain = ink_points(&font, "|", [1.0, 1.0, 1.0]);
        assert!(!plain.is_empty());
        let (x0, _, x1, _) = bbox(&plain);
        assert!(
            x1 - x0 <= 1.5,
            "a plain (non-italic) '|' must be a single narrow column across every \
             row; measured width {} (bbox x {x0}..{x1})",
            x1 - x0
        );
    }

    /// **Underline / strikethrough**: `Font.java:274,285-299` draws a 1px bar
    /// per glyph, spanning that glyph's advance (extended 1px left for the
    /// *first* glyph of the run) — unconditionally, including for a space,
    /// which has no ink of its own. Using two spaces isolates the bar
    /// completely: there is no glyph ink anywhere to confuse it with.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn underlined_spaces_draw_the_bar_with_no_glyph_ink_to_confuse_it() {
        let font = font();
        let plain_spaces = ink_points(&font, "  ", [1.0, 1.0, 1.0]);
        assert!(
            plain_spaces.is_empty(),
            "control: plain spaces must draw no ink at all — found {} vertices",
            plain_spaces.len()
        );

        let underlined = ink_points(&font, "\u{a7}n  ", [1.0, 1.0, 1.0]);
        assert!(
            !underlined.is_empty(),
            "underlined spaces must still draw the underline bar"
        );
        let (x0, y0, x1, y1) = bbox(&underlined);

        let space_advance = font
            .raster
            .advance(' ' as u32)
            .expect("space must be covered");
        let want_x0 = ORIGIN_X - font_metrics::EFFECT_LEAD_IN;
        let want_x1 = ORIGIN_X + 2.0 * space_advance;
        let want_y0 = ORIGIN_Y + font_metrics::UNDERLINE_Y - font_metrics::EFFECT_THICKNESS;
        let want_y1 = ORIGIN_Y + font_metrics::UNDERLINE_Y;

        eprintln!(
            "underline bbox: x {x0:.2}..{x1:.2}, y {y0:.2}..{y1:.2}; want x \
             {want_x0:.2}..{want_x1:.2}, y {want_y0:.2}..{want_y1:.2}"
        );
        assert!((x0 - want_x0).abs() < 1e-2, "x0 {x0} != {want_x0}");
        assert!((x1 - want_x1).abs() < 1e-2, "x1 {x1} != {want_x1}");
        assert!((y0 - want_y0).abs() < 1e-2, "y0 {y0} != {want_y0}");
        assert!((y1 - want_y1).abs() < 1e-2, "y1 {y1} != {want_y1}");
    }

    /// As above, for strikethrough, which sits at a different fixed offset
    /// (`Font.java:285-291`, `STRIKETHROUGH_Y` vs `UNDERLINE_Y`) — the two
    /// must land at *different* y, not share one "there is a line somewhere"
    /// implementation.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn strikethrough_spaces_land_at_a_different_y_than_underline() {
        let font = font();
        let struck = ink_points(&font, "\u{a7}m  ", [1.0, 1.0, 1.0]);
        assert!(!struck.is_empty());
        let (x0, y0, x1, y1) = bbox(&struck);

        let space_advance = font
            .raster
            .advance(' ' as u32)
            .expect("space must be covered");
        let want_x0 = ORIGIN_X - font_metrics::EFFECT_LEAD_IN;
        let want_x1 = ORIGIN_X + 2.0 * space_advance;
        let want_y0 = ORIGIN_Y + font_metrics::STRIKETHROUGH_Y - font_metrics::EFFECT_THICKNESS;
        let want_y1 = ORIGIN_Y + font_metrics::STRIKETHROUGH_Y;

        assert!((x0 - want_x0).abs() < 1e-2, "x0 {x0} != {want_x0}");
        assert!((x1 - want_x1).abs() < 1e-2, "x1 {x1} != {want_x1}");
        assert!((y0 - want_y0).abs() < 1e-2, "y0 {y0} != {want_y0}");
        assert!((y1 - want_y1).abs() < 1e-2, "y1 {y1} != {want_y1}");
        assert_ne!(
            want_y0,
            ORIGIN_Y + font_metrics::UNDERLINE_Y - font_metrics::EFFECT_THICKNESS,
            "strikethrough and underline must not share a y offset"
        );
    }

    /// A colour code (not just `§r`) resets bold — `apply_legacy_code`
    /// (`lodestone-model/src/text.rs:626-644`): "a legacy colour code resets
    /// all formatting to just that colour". `§r` is the easy half to get
    /// right; a colour code doing the same is the part a naive
    /// "only reset on `§r`" implementation misses.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn a_bare_r_resets_bold_before_the_next_glyph() {
        let font = font();
        let plain_w = width_of(&ink_points(&font, "l", [1.0, 1.0, 1.0]));
        let bold_w = width_of(&ink_points(&font, "\u{a7}ll", [1.0, 1.0, 1.0]));
        let reset_w = width_of(&ink_points(&font, "\u{a7}l\u{a7}rl", [1.0, 1.0, 1.0]));
        assert_ne!(
            bold_w, plain_w,
            "sanity: bold must actually be wider before trusting the reset check"
        );
        assert!(
            (reset_w - plain_w).abs() < 1e-3,
            "§r before a glyph must clear bold: expected width {plain_w} (plain), got \
             {reset_w} (bold width would be {bold_w})"
        );
    }

    /// **Obfuscated**: `Font.getGlyph` (`Font.java:82-91`) swaps in a random
    /// same-width-class glyph every time it is asked, from a `RandomSource`
    /// that is never reseeded (`Font.java:34`) — so two draws of the *same*
    /// `§k` string must produce **different** ink, which is what makes it read
    /// as animated rather than a one-shot scramble. A still frame cannot
    /// distinguish "animated" from "static but wrong", so this compares two
    /// draws and requires them to differ — with a determinism control proving
    /// the comparison itself is capable of finding "equal" when the input
    /// really is unstyled.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn obfuscated_text_resamples_between_draws() {
        let font = font();

        // Control: plain text is perfectly deterministic across draws — if
        // this fails, `ink_points` itself is unstable and the difference
        // asserted below would be meaningless.
        let plain_a = ink_points(&font, "ABCDEFGHIJ", [1.0, 1.0, 1.0]);
        let plain_b = ink_points(&font, "ABCDEFGHIJ", [1.0, 1.0, 1.0]);
        assert_eq!(
            plain_a, plain_b,
            "control: plain text must draw identically twice"
        );

        let obf_a = ink_points(&font, "\u{a7}kABCDEFGHIJ", [1.0, 1.0, 1.0]);
        let obf_b = ink_points(&font, "\u{a7}kABCDEFGHIJ", [1.0, 1.0, 1.0]);
        assert!(!obf_a.is_empty());
        assert_ne!(
            obf_a, obf_b,
            "obfuscated text must resample its glyphs on every draw call — two \
             draws of the identical `§k` string produced identical ink"
        );
        // Layout must not move even though pixels do: the advance is always
        // the *original* codepoint's, never the substitute's (see
        // `obfuscation_pool`'s field docs).
        assert_eq!(
            width_of(&obf_a),
            width_of(&obf_b),
            "obfuscation must resample pixels, not advance/layout"
        );
    }

    /// Space is never obfuscated (`Font.java:85`, `codepoint != 32`) — an
    /// obfuscated string with spaces in it must keep them as gaps, not
    /// replace them with visible ink.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn obfuscated_space_stays_a_gap() {
        let font = font();
        let obf = ink_points(&font, "\u{a7}kA B", [1.0, 1.0, 1.0]);
        assert!(!obf.is_empty());
        let a_advance = font.raster.advance('A' as u32).unwrap();
        let space_advance = font.raster.advance(' ' as u32).unwrap();
        let gap_x0 = ORIGIN_X + a_advance;
        let gap_x1 = gap_x0 + space_advance;
        let in_gap = obf
            .iter()
            .filter(|(x, _)| *x >= gap_x0 && *x < gap_x1)
            .count();
        assert_eq!(
            in_gap, 0,
            "the space between the two obfuscated letters must stay empty, found \
             {in_gap} ink vertices in x {gap_x0:.1}..{gap_x1:.1}"
        );
    }
}

/// Colour gates for the **real** `draw_spans` path.
///
/// ## Why this module exists separately from `tests/text_colour.rs`
///
/// It exists because the integration gate, on its own, was **vacuous for this
/// path** — and that was discovered by running a neuter, not by reading it.
/// `HudGeometry::build` attaches no [`VanillaFont`] (the font lives on
/// `HudRenderer`, not on the geometry builder), so every draw it makes takes
/// `Builder::text_spans`' jar-less fixed-advance fallback. Replacing
/// `spans_run`'s per-span colour with the base colour outright — the exact
/// pre-fix defect — left all nine integration tests green, because the neutered
/// function was never called.
///
/// That is the *world* species of vacuous test: the source is fine and the flaw
/// is in what it was pointed at. A `client.jar` was present the whole time; the
/// harness simply could not reach the code it was written to check.
///
/// So the split is: `tests/text_colour.rs` covers the model, the projections and
/// the fallback draw with no jar and no adapter, in the default test run; this
/// module covers the vanilla path that real players see. `#[ignore]`d for the jar,
/// per the sibling `styling_tests` convention — a missing jar is a **fail-closed**
/// precondition here, never a skip.
///
/// ```text
/// cargo test -p lodestone-shell --lib -- --ignored --nocapture span_colour_tests
/// ```
#[cfg(test)]
mod span_colour_tests {
    use super::*;
    use lodestone_model::text::TextStyle;

    const W: f32 = 400.0;
    const H: f32 = 200.0;

    /// Vanilla's sixteen, hand-transcribed from `TextColor.java:18-33` (26.2).
    /// Deliberately **not** built from [`TextColor::rgb`] — that is the code under
    /// test, and an expectation derived from it would be satisfied by all sixteen
    /// being wrong together.
    const VANILLA: [(&str, u32, TextColor); 16] = [
        ("black", 0x0000_0000, TextColor::Black),
        ("dark_blue", 0x0000_00aa, TextColor::DarkBlue),
        ("dark_green", 0x0000_aa00, TextColor::DarkGreen),
        ("dark_aqua", 0x0000_aaaa, TextColor::DarkAqua),
        ("dark_red", 0x00aa_0000, TextColor::DarkRed),
        ("dark_purple", 0x00aa_00aa, TextColor::DarkPurple),
        ("gold", 0x00ff_aa00, TextColor::Gold),
        ("gray", 0x00aa_aaaa, TextColor::Gray),
        ("dark_gray", 0x0055_5555, TextColor::DarkGray),
        ("blue", 0x0055_55ff, TextColor::Blue),
        ("green", 0x0055_ff55, TextColor::Green),
        ("aqua", 0x0055_ffff, TextColor::Aqua),
        ("red", 0x00ff_5555, TextColor::Red),
        ("light_purple", 0x00ff_55ff, TextColor::LightPurple),
        ("yellow", 0x00ff_ff55, TextColor::Yellow),
        ("white", 0x00ff_ffff, TextColor::White),
    ];

    /// A real vanilla font, or a loud failure — never a silent skip.
    fn font() -> VanillaFont {
        let manager = crate::resources::vanilla_manager().expect(
            "span colour gate opted in via --ignored but no vanilla client.jar was found; \
             set LODESTONE_ASSETS to a pack root containing client.jar, or populate \
             .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
        );
        VanillaFont::from_manager(&manager).expect("build the vanilla font")
    }

    /// `round`, not a truncating cast: `170.0 / 255.0 * 255.0` is `169.99999` in
    /// binary floating point, and truncation would turn every `0xAA` channel into
    /// `0xA9` and fail every assertion here for a reason unrelated to colour.
    fn as_byte(v: f32) -> u8 {
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    }

    fn unpack(hex: u32) -> (u8, u8, u8) {
        (
            ((hex >> 16) & 0xff) as u8,
            ((hex >> 8) & 0xff) as u8,
            (hex & 0xff) as u8,
        )
    }

    fn span(text: &str, color: Option<TextColor>) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            style: TextStyle {
                color,
                ..TextStyle::default()
            },
        }
    }

    /// Draw `spans` through the real [`VanillaFont::draw_spans`] and return every
    /// emitted vertex as `(x, y, rgb-bytes)` in local pixel space.
    ///
    /// Both passes are kept. The shadow is a quarter of its run's colour, and no
    /// named colour is a quarter of another (`0xAA/4 = 0x2A`, `0xFF/4 = 0x3F`,
    /// `0x55/4 = 0x15`, and none of `0x00`/`0x55`/`0xAA`/`0xFF` equals any of
    /// those), so a shadow can never be mistaken for a named colour. Black is its
    /// own shadow, which is harmless for a presence test.
    fn emitted(font: &VanillaFont, spans: &[TextSpan], base: [f32; 3]) -> Vec<(f32, f32, (u8, u8, u8))> {
        let mut verts = Vec::new();
        {
            let mut cs = ColourStream {
                verts: &mut verts,
                w: W,
                h: H,
            };
            font.draw_spans(&mut cs, spans, 50.0, 50.0, 1.0, base, 1.0);
        }
        verts
            .chunks_exact(6)
            .map(|v| {
                (
                    (v[0] + 1.0) * W * 0.5,
                    (1.0 - v[1]) * H * 0.5,
                    (as_byte(v[2]), as_byte(v[3]), as_byte(v[4])),
                )
            })
            .collect()
    }

    /// Bounding box of every vertex carrying `rgb` — failure output says *where*,
    /// not what fraction, so a localised blob is distinguishable from a uniform
    /// miss.
    fn bbox_of(ink: &[(f32, f32, (u8, u8, u8))], rgb: (u8, u8, u8)) -> Option<(f32, f32, f32, f32)> {
        let hits: Vec<_> = ink.iter().filter(|(_, _, c)| *c == rgb).collect();
        if hits.is_empty() {
            return None;
        }
        Some((
            hits.iter().map(|h| h.0).fold(f32::INFINITY, f32::min),
            hits.iter().map(|h| h.1).fold(f32::INFINITY, f32::min),
            hits.iter().map(|h| h.0).fold(f32::NEG_INFINITY, f32::max),
            hits.iter().map(|h| h.1).fold(f32::NEG_INFINITY, f32::max),
        ))
    }

    fn distinct(ink: &[(f32, f32, (u8, u8, u8))]) -> Vec<(u8, u8, u8)> {
        let mut v: Vec<(u8, u8, u8)> = ink.iter().map(|(_, _, c)| *c).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// **The gate.** All sixteen named colours must reach a quad at vanilla's
    /// exact bytes, through the font a player actually sees.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn each_named_colour_draws_at_its_exact_vanilla_bytes() {
        let font = font();
        // A base colour that is none of the sixteen, so a run that silently fell
        // back to `base` could not be mistaken for a correctly-coloured one.
        let base = [0.1, 0.2, 0.3];
        let spans: Vec<TextSpan> = VANILLA
            .iter()
            .map(|(_, _, c)| span("M", Some(*c)))
            .collect();
        let ink = emitted(&font, &spans, base);
        assert!(
            !ink.is_empty(),
            "draw_spans emitted no vertices at all, so this gate would pass \
             vacuously for any colour"
        );

        let mut missing = Vec::new();
        for (name, hex, _) in VANILLA {
            if bbox_of(&ink, unpack(hex)).is_none() {
                missing.push(format!("{name} #{hex:06x} {:?}", unpack(hex)));
            }
        }
        assert!(
            missing.is_empty(),
            "these vanilla colours never reached a quad: {missing:?}\n\
             colours actually emitted: {:?}",
            distinct(&ink)
        );
    }

    /// **Negative control, executed.** The same sixteen expectations against
    /// uncoloured spans must fail — the detector demonstrating it can say "no".
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn control_uncoloured_spans_produce_no_named_colour() {
        let font = font();
        let base = [0.1, 0.2, 0.3];
        let spans: Vec<TextSpan> = (0..16).map(|_| span("M", None)).collect();
        let ink = emitted(&font, &spans, base);
        assert!(
            !ink.is_empty(),
            "the control must still draw ink, or it fails for the wrong reason"
        );

        let found: Vec<&str> = VANILLA
            .iter()
            .filter(|(_, hex, _)| bbox_of(&ink, unpack(*hex)).is_some())
            .map(|(name, _, _)| *name)
            .collect();
        assert!(
            found.is_empty(),
            "uncoloured spans must draw only in `base`, but produced named \
             colours {found:?}; emitted: {:?}",
            distinct(&ink)
        );
    }

    /// A hex colour reaches its exact bytes. This is the case the legacy `§` path
    /// structurally cannot express, so a suite built only from the sixteen named
    /// colours is blind to it.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn a_hex_span_draws_at_its_exact_bytes() {
        let font = font();
        let hex = 0x001f_2e3d;
        let want = unpack(hex);
        let ink = emitted(&font, &[span("M", Some(TextColor::Rgb(hex)))], [1.0, 1.0, 1.0]);
        assert!(
            bbox_of(&ink, want).is_some(),
            "hex #{hex:06x} {want:?} never reached a quad; emitted: {:?}",
            distinct(&ink)
        );
    }

    /// An uncoloured span draws in `base`, which is what makes the styled path
    /// behaviour-preserving for text a server never coloured.
    #[test]
    #[ignore = "requires the vanilla client.jar"]
    fn an_uncoloured_span_draws_in_the_base_colour() {
        let font = font();
        let base = [1.0, 170.0 / 255.0, 0.0];
        let ink = emitted(&font, &[span("M", None)], base);
        let want = (255u8, 170u8, 0u8);
        assert!(
            bbox_of(&ink, want).is_some(),
            "an uncoloured span must draw in base {want:?}; emitted: {:?}",
            distinct(&ink)
        );
    }
}
