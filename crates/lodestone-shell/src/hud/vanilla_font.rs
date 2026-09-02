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
//!   [`draw_legacy`](VanillaFont::draw_legacy) — see
//!   [`resolve_legacy`](VanillaFont::resolve_legacy) and
//!   [`glyph_styled`](VanillaFont::glyph_styled). This used to be the one
//!   documented gap in this module: the metrics existed
//!   (`Font::advance_bold`, `metrics::ITALIC_SHEAR`) and `§l`/`§o`/`§n`/`§m`/`§k`
//!   were parsed for **width** (`Font::legacy_width`), but the draw side treated
//!   every flag as zero-width state and dropped it, so a bold or italic name
//!   measured correctly and drew as plain text. [`run`](VanillaFont::run) (the
//!   unstyled path `draw`/`draw_plain` use) is untouched: a plain `&str` carries
//!   no `§` codes, so it can never be styled, and giving it the styled glyph path
//!   anyway would cost every title/subtitle/XP-number draw a pool lookup for
//!   nothing.
//! * **`unihex` now rasterises; `ttf` still does not.** `unifont.zip`'s 114,432
//!   glyphs come through [`lodestone_assets::font::GlyphRaster`] like a sheet
//!   cell, at `texel_size` 0.5 instead of 1.0, so [`draw_ink`](VanillaFont::draw_ink)
//!   needed no change at all — there is still no atlas and no fifth bind group.
//!   The codepoints that remain missing-glyph boxes are the ones neither the
//!   three sheets nor `unifont.zip` cover: astral-plane emoji, and anything a
//!   `ttf` provider would have supplied. See [`jar_manager`] for why this needs
//!   the asset-object store and not just the jar.
//! * **A unihex glyph's shadow now lags by its own 0.5 px, not the sheet
//!   default's 1 px.** [`draw_legacy`](VanillaFont::draw_legacy) and
//!   [`draw_spans`](VanillaFont::draw_spans) used to add one offset before
//!   either pass began, which was only correct because every glyph shared it;
//!   [`draw_resolved`](VanillaFont::draw_resolved) now looks up
//!   [`Font::shadow_offset`](lodestone_assets::font::Font::shadow_offset)
//!   per glyph, matching bold's second pass (already per-glyph via
//!   `Font::bold_offset`) rather than trailing it.
//! * **Right-to-left runs are now reordered for display; shaping is not.**
//!   [`bidi_reorder`] applies the paragraph-level and explicit-embedding rules
//!   of the Unicode Bidirectional Algorithm (UAX #9) to lay Arabic/Hebrew runs
//!   right-to-left among any surrounding LTR text, mirroring vanilla's
//!   `Language.getVisualOrder` (`Bidi`-backed). It reorders **codepoints**, not
//!   glyphs: Arabic's per-position joining forms (isolated/initial/medial/
//!   final) are not selected, so a reordered Arabic run draws its isolated-form
//!   glyphs in the right left-to-right screen order rather than the wrong one,
//!   but does not yet cursively join them. Every draw entry point
//!   ([`draw`](VanillaFont::draw), [`draw_legacy`](VanillaFont::draw_legacy),
//!   [`draw_spans`](VanillaFont::draw_spans), [`draw_plain`](VanillaFont::draw_plain))
//!   reorders before laying out glyphs, so a caller never sees the bidi step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use lodestone_assets::font::{
    FontLoader, FontOptions, GlyphRaster, MISSING_ADVANCE, RasterFont, metrics as font_metrics,
};
use lodestone_assets::{ResourceLocation, ResourceManager};
use lodestone_model::text::{FontId, TextColor, TextSpan};

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
/// (`lodestone_model::text::apply_legacy_code`: "a legacy
/// colour code resets all formatting to just that colour"); this type carries
/// no colour of its own for that reason —
/// [`resolve_legacy`](VanillaFont::resolve_legacy) tracks colour separately
/// and clears both together.
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

/// One decoded, drawable glyph: its visible character, active style and
/// resolved colour — everything
/// [`draw_resolved`](VanillaFont::draw_resolved) needs to lay it out. Built by
/// [`resolve_legacy`](VanillaFont::resolve_legacy)/
/// [`resolve_spans`](VanillaFont::resolve_spans) in **logical** (source)
/// order, then permuted in place by [`bidi_reorder_glyphs`] into **visual**
/// (left-to-right screen) order before any drawing happens. `Copy` because
/// [`bidi_reorder_glyphs`] rebuilds the list by indexing rather than moving.
#[derive(Debug, Clone, Copy)]
struct ResolvedGlyph {
    ch: char,
    style: GlyphStyle,
    rgb: [f32; 3],
    /// The `"font": "<ns>:<name>"` this run's [`lodestone_model::text::TextStyle`]
    /// resolved to, if any — `None` means the default font (also what a
    /// `§`-coded run decoded by [`VanillaFont::resolve_legacy`] always
    /// carries, since legacy codes have no font of their own; see that
    /// method's doc). [`VanillaFont::select_font`] is where this is actually
    /// consulted, per glyph, with a fallback to the default font when the
    /// named font does not cover this glyph's codepoint.
    font: Option<FontId>,
}

/// Reorders `glyphs` in place for display, applying the Unicode
/// Bidirectional Algorithm (UAX #9) to the codepoints it already holds —
/// vanilla's `Language.getVisualOrder`. A no-op for the overwhelmingly common
/// case (every built-in string, most chat) where every character is ASCII,
/// since a default-LTR paragraph with no bidi classes beyond `L`/neutral
/// reorders to itself; the full algorithm only runs once a non-ASCII
/// character is actually present.
///
/// **This reorders codepoints, not glyphs.** Arabic's per-position joining
/// forms (isolated/initial/medial/final) are not selected — a reordered
/// Arabic run draws its isolated-form glyphs in the correct left-to-right
/// screen order, but does not cursively join them. Shaping is a separate,
/// unimplemented pass; see this module's own doc.
fn bidi_reorder_glyphs(glyphs: &mut Vec<ResolvedGlyph>) {
    if glyphs.len() < 2 {
        return;
    }
    // Checked on `glyphs` directly, before building `text`, so the
    // overwhelmingly common all-ASCII case (every frame's worth of Latin
    // chat and every built-in string) pays for an `all()` scan and nothing
    // else — no allocation, no `unicode_bidi::BidiInfo` construction.
    if glyphs.iter().all(|g| g.ch.is_ascii()) {
        return;
    }
    let text: String = glyphs.iter().map(|g| g.ch).collect();
    let order = bidi_visual_order(&text);
    if order.iter().enumerate().all(|(i, &j)| i == j) {
        return;
    }
    let original = std::mem::take(glyphs);
    glyphs.extend(order.into_iter().map(|i| original[i]));
}

/// The **char**-index permutation that puts `text`'s codepoints into
/// left-to-right screen order per UAX #9: `order[visual_position]` is the
/// logical (source) char index that belongs there.
///
/// `unicode_bidi`'s own levels and ranges are byte-indexed (a multi-byte
/// codepoint repeats its level across its bytes); `BidiInfo::reorder_visual`
/// implements rule L2 over an arbitrary per-*unit* level array and is
/// documented to want exactly one [`unicode_bidi::Level`] per codepoint for
/// that reason, which `BidiInfo::reordered_levels_per_char` (rule L1 already
/// applied) provides directly — so this needs no manual byte-to-char mapping
/// of its own.
fn bidi_visual_order(text: &str) -> Vec<usize> {
    use unicode_bidi::BidiInfo;

    let bidi_info = BidiInfo::new(text, None);
    if bidi_info.paragraphs.len() > 1 {
        // An embedded hard paragraph break (e.g. a multi-line kick reason).
        // UAX #9 never reorders paragraphs relative to each other, only the
        // characters inside one, so each is resolved independently against
        // its own substring and the results are concatenated in source
        // order. This recurses at most once per paragraph: a paragraph's own
        // `range` spans exactly one paragraph, so the recursive call always
        // hits the `len() <= 1` arm below.
        let mut order = Vec::with_capacity(text.chars().count());
        let mut char_base = 0usize;
        for para in &bidi_info.paragraphs {
            let slice = &text[para.range.clone()];
            order.extend(bidi_visual_order(slice).into_iter().map(|i| i + char_base));
            char_base += slice.chars().count();
        }
        return order;
    }
    let Some(para) = bidi_info.paragraphs.first() else {
        return Vec::new();
    };
    let levels = bidi_info.reordered_levels_per_char(para, para.range.clone());
    BidiInfo::reorder_visual(&levels)
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
    ///. Vanilla's own pool is built from *every*
    /// active provider (including `space`), but only bitmap glyphs are
    /// drawable here, so this is restricted to codepoints
    /// [`RasterFont::raster`] actually returns pixels for — a codepoint with no
    /// ink never gets picked as a replacement, which is the one place this
    /// diverges from vanilla's pool (documented, not a bug: it would otherwise
    /// occasionally "obfuscate" a glyph into invisible whitespace).
    obfuscation_pool: HashMap<u32, Vec<char>>,
    /// Free-running state for the obfuscated-glyph picker. Vanilla never
    /// reseeds `Font.random` (`Font.java`, `RandomSource.create()` at
    /// construction) — every glyph drawn advances the same stream, so the
    /// same text resamples differently **every frame** with no timer
    /// involved, which is what makes `§k` read as animated rather than a
    /// one-shot randomised label. `AtomicU64` reproduces that: `&self` is
    /// shared (this font lives behind an `Arc`), so the state has to be
    /// interior-mutable, and every glyph draw advances it exactly once, same
    /// as vanilla.
    obfuscation_rng: AtomicU64,
    /// Source-colour scanline runs for glyphs already drawn by this font.
    ///
    /// The old path revisited every source texel for every glyph, twice per
    /// draw (shadow + ink) and again on the next frame. F3 makes that scan one
    /// of the hottest functions in the client. Runs are independent of scale,
    /// position and tint, so one entry can serve every HUD surface while still
    /// letting [`Self::draw_ink`] apply those per-draw values exactly.
    ink_runs: Mutex<HashMap<InkRunCacheKey, Arc<CachedGlyphInk>>>,
    /// Custom fonts a `"font": "<ns>:<name>"` span can name, loaded lazily
    /// from the same resource-pack-aware manager the default font uses (see
    /// [`jar_manager`]) and cached — `None` for an id that failed to load, so
    /// a malformed or absent pack font is not re-resolved every frame it
    /// appears in. The whole cache is invalidated when the resource-pack
    /// generation changes: this `VanillaFont` survives a server pack's
    /// asynchronous installation, while its prior negative lookup must not.
    /// See [`Self::select_font`].
    custom: Mutex<CustomFontCache>,
}

/// Custom-font entries resolved against one exact resource-pack stack.
///
/// A pack-generation change can add, remove or override any font, so retaining
/// either a successful or failed lookup across it is wrong. Keeping a single
/// generation instead of keying every entry by generation bounds the cache when
/// a player changes packs repeatedly during one session. This is the shared
/// [`crate::resources::pack_generation`] signal, so a mipmap-only change also
/// conservatively clears this tiny cache; that may reload a font once, but
/// needs no second cross-subsystem generation counter.
#[derive(Debug, Default)]
struct CustomFontCache {
    generation: Option<u64>,
    entries: HashMap<FontId, Option<Arc<RasterFont>>>,
}

/// Identity of one raster font/codepoint pair in [`VanillaFont::ink_runs`].
///
/// `RasterFont` has no public asset id because a custom font can be an
/// in-memory fixture or a pack override. Its address is stable for its
/// lifetime: the default raster is owned by `VanillaFont`, and custom rasters
/// are retained by [`CustomFontCache`]. A pack change replaces either owner,
/// producing a different identity; the old run is harmless and disappears
/// with this `VanillaFont` or its small cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct InkRunCacheKey {
    raster: usize,
    codepoint: u32,
}

/// One horizontal run of identical, non-transparent source texels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CachedInkRun {
    ty: u32,
    start: u32,
    end: u32,
    source: [f32; 4],
}

/// Tint-independent drawing data derived from one [`GlyphRaster`] walk.
#[derive(Debug)]
struct CachedGlyphInk {
    texel_size: f32,
    top: f32,
    left: f32,
    advance: f32,
    runs: Vec<CachedInkRun>,
}

impl CachedGlyphInk {
    fn from_raster(raster: &GlyphRaster<'_>) -> Self {
        let mut runs = Vec::new();
        for ty in 0..raster.cell_height() {
            let mut tx = 0;
            while tx < raster.cell_width() {
                let source = raster.texel_rgba(tx, ty);
                if source[3] == 0.0 {
                    tx += 1;
                    continue;
                }
                let start = tx;
                tx += 1;
                while tx < raster.cell_width() && raster.texel_rgba(tx, ty) == source {
                    tx += 1;
                }
                runs.push(CachedInkRun {
                    ty,
                    start,
                    end: tx,
                    source,
                });
            }
        }
        Self {
            texel_size: raster.texel_size(),
            top: raster.top(),
            left: raster.left(),
            advance: raster.advance(),
            runs,
        }
    }
}

/// Process-wide cache, **keyed on the pack generation**. The jar is ~37 MB and
/// the font is tiny; loading it once keeps `HudRenderer::new` cheap even when a
/// test builds several renderers.
///
/// It used to be a `OnceLock`, and that was a real defect rather than a
/// simplification: the first caller in the process decided the default font for
/// the whole session, so a pack applied *afterwards* — a server-pushed pack, or
/// any change made on the Resource Packs screen — could never replace
/// `minecraft:default`. Custom fonts were unaffected, because
/// [`CustomFontCache`] right above this already keys on the generation, which is
/// the shape "the pack's fonts work in some places and not in chat" takes from
/// the outside.
///
/// Holding an `Option` inside an `Option` is deliberate: the outer one is "no
/// reading for this generation yet" and the inner one is "this generation has no
/// font", which is a real, cached answer for a jar-less run and must not be
/// retried every frame.
static SHARED: Mutex<Option<(u64, Option<Arc<VanillaFont>>)>> = Mutex::new(None);

/// Re-resolve a held default font if the resource-pack stack has changed since
/// it was resolved, reporting whether it moved.
///
/// **One implementation, because there are three holders.** `HudRenderer`,
/// `MenuRenderer` and `ContainerRenderer` each resolve
/// [`VanillaFont::shared`] once in their own `new` and each has the same
/// staleness problem; this repo's record says a fix discovered twice and
/// written twice is how the second call site keeps the bug, so the compare is
/// here and the holders keep only the two fields.
///
/// `generation` is the caller's stamp and is updated in place. An unchanged
/// generation costs one relaxed atomic load and an integer compare, which is
/// why this is safe to call every frame — there is no other seam that runs on a
/// renderer when a pack lands, because a server-pushed pack installs on the
/// network thread and the Resource Packs screen writes the selection directly.
///
/// A new stack resolving to `None` is assigned, not ignored: a pack that breaks
/// the font must fall back to the fixed-advance debug font rather than keep
/// drawing the previous pack's glyphs.
pub fn refresh_shared_font(font: &mut Option<Arc<VanillaFont>>, generation: &mut u64) -> bool {
    refresh_shared_font_to(font, generation, crate::resources::pack_generation())
}

/// [`refresh_shared_font`] against an explicit `current` generation — the
/// hermetic seam, the same one [`VanillaFont::custom_raster_for_generation`] is
/// for custom fonts.
///
/// It exists because `crate::resources::pack_generation` is **process-wide
/// mutable state shared with every other test in the binary**: a gate written
/// against the live counter can be moved under its own feet by a concurrently
/// running test that selects a pack, which is a flake, not a finding.
pub fn refresh_shared_font_to(
    font: &mut Option<Arc<VanillaFont>>,
    generation: &mut u64,
    current: u64,
) -> bool {
    if current == *generation {
        return false;
    }
    *generation = current;
    *font = VanillaFont::shared_for_generation(current);
    true
}

/// The id [`FontId::name`] returns for the vanilla default font — never
/// worth a pack-stack lookup of its own since [`VanillaFont`] already carries
/// it as [`VanillaFont::raster`].
const DEFAULT_FONT_NAME: &str = "minecraft:default";

impl VanillaFont {
    /// The process-wide vanilla font, loaded on first call from the same pack
    /// the other vanilla atlases come from.
    ///
    /// Fail-open by design: a jar-less run (headless gates, the demo world)
    /// gets `None` and every caller keeps the fixed-width fallback, which is
    /// what makes this module safe to wire in unconditionally.
    #[must_use]
    pub fn shared() -> Option<Arc<VanillaFont>> {
        Self::shared_for_generation(crate::resources::pack_generation())
    }

    /// [`Self::shared`] against an explicit pack generation — the hermetic seam,
    /// exactly as [`Self::custom_raster_for_generation`] is for custom fonts, so
    /// the cache policy can be tested without mutating process-wide pack state.
    ///
    /// A caller that *holds* the result must record the generation it asked for
    /// and re-ask when it moves; an `Arc` handed out here is a snapshot, and
    /// keeping one across a pack change is precisely the bug this replaced.
    /// [`crate::hud::HudRenderer::refresh_font_for_pack_generation`] is the
    /// worked example.
    #[must_use]
    pub fn shared_for_generation(generation: u64) -> Option<Arc<VanillaFont>> {
        let mut guard = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((cached, font)) = guard.as_ref()
            && *cached == generation
        {
            return font.clone();
        }
        let font = Self::load();
        *guard = Some((generation, font.clone()));
        font
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
                    // The number that says whether CJK/Thai/Arabic draw at all;
                    // 0 means the asset-object store did not resolve and the
                    // jar's empty `unifont.json` stub won. See `jar_manager`.
                    unihex = raster.unihex_count(),
                    "loaded the vanilla default font for the HUD"
                );
                let obfuscation_pool = build_obfuscation_pool(&raster);
                Some(Arc::new(VanillaFont {
                    raster,
                    obfuscation_pool,
                    obfuscation_rng: AtomicU64::new(0x9E37_79B9_7F4A_7C15),
                    ink_runs: Mutex::new(HashMap::new()),
                    custom: Mutex::new(CustomFontCache::default()),
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
            ink_runs: Mutex::new(HashMap::new()),
            custom: Mutex::new(CustomFontCache::default()),
        })
    }

    /// Resolves and caches a custom font by id, loaded from the same
    /// resource-pack-aware manager the default font uses ([`jar_manager`],
    /// which layers the selected local packs and a live server-pushed pack
    /// over the jar — see that function's own doc for why a browser session's
    /// jar bytes are only reachable that way, and why a server pack rides
    /// along). `None` on first failure is cached too, matching every other
    /// pack loader's fail-open discipline: a malformed or absent pack font
    /// must degrade to the default font, not be retried every frame it
    /// appears in a message, and never panic — the pack is untrusted input.
    fn custom_raster(&self, id: FontId) -> Option<Arc<RasterFont>> {
        self.custom_raster_for_generation(id, crate::resources::pack_generation(), || {
            load_custom_font(id.name())
        })
    }

    /// The active default raster for a world-text surface. The shared font is
    /// keyed by `resources::pack_generation`, so asking [`Self::shared`] each
    /// frame makes a server-pack reload replace this answer rather than leaving
    /// a jar-only snapshot alive.
    pub(crate) fn default_raster(&self) -> &RasterFont {
        &self.raster
    }

    /// Resolve one non-default component font from the active pack stack.
    /// World-space text uses the same lazy, generation-invalidated cache as the
    /// HUD; the caller still performs vanilla's per-codepoint coverage fallback.
    pub(crate) fn custom_raster_for_world_text(&self, id: FontId) -> Option<Arc<RasterFont>> {
        self.custom_raster(id)
    }

    /// Resolves one custom font against `generation`'s pack stack.
    ///
    /// Kept separate from [`Self::custom_raster`] so the cache policy has a
    /// hermetic test seam: tests can change the generation and make a synthetic
    /// font available without mutating the process-wide resource-pack state or
    /// requiring a `client.jar`.
    fn custom_raster_for_generation<F>(
        &self,
        id: FontId,
        generation: u64,
        load: F,
    ) -> Option<Arc<RasterFont>>
    where
        F: FnOnce() -> Option<Arc<RasterFont>>,
    {
        if id.name() == DEFAULT_FONT_NAME {
            return None;
        }
        let mut cache = recover_poisoned_lock(self.custom.lock());
        if cache.generation != Some(generation) {
            cache.generation = Some(generation);
            cache.entries.clear();
            // `InkRunCacheKey` uses a raster allocation's address. Dropping
            // the old custom-font Arcs permits a later generation to reuse
            // that address, so its derived pixels/metrics must disappear in
            // the same invalidation step. Clearing the default-font runs too
            // is conservative and happens only on a pack-generation change.
            recover_poisoned_lock(self.ink_runs.lock()).clear();
        }
        if let Some(entry) = cache.entries.get(&id) {
            return entry.clone();
        }
        // Font opens are rare (only on the first glyph for a font/generation),
        // and the mutex protects only this small cache. Keep it while loading
        // so concurrent nameplates share this one result and one warning.
        let loaded = load();
        cache.entries.insert(id, loaded.clone());
        loaded
    }

    /// Which [`RasterFont`] actually supplies `cp`'s pixels: `custom` when it
    /// declares coverage for that codepoint, else the default. This is the
    /// per-glyph fallback issue #679 asks for — a pack font that only defines
    /// a handful of icon codepoints still draws ordinary Latin text from the
    /// default font rather than the missing-glyph box, while a codepoint the
    /// custom font *does* define wins even where the default font would also
    /// have drawn something there (matching vanilla's own provider-priority
    /// rule, extended across the font boundary rather than just within one
    /// font's provider list).
    fn select_font<'a>(&'a self, cp: u32, custom: Option<&'a RasterFont>) -> &'a RasterFont {
        if let Some(custom) = custom
            && custom.font().contains(cp)
        {
            return custom;
        }
        &self.raster
    }

    /// Return one glyph's tint-independent scanline runs, building them only
    /// on the first draw. The lookup happens before [`RasterFont::raster`], so
    /// cached TTF glyphs avoid both the texel scan and fontdue's rasterisation.
    fn cached_ink(&self, font: &RasterFont, codepoint: u32) -> Option<Arc<CachedGlyphInk>> {
        let key = InkRunCacheKey {
            raster: font as *const RasterFont as usize,
            codepoint,
        };
        if let Some(ink) = recover_poisoned_lock(self.ink_runs.lock()).get(&key) {
            return Some(Arc::clone(ink));
        }

        let raster = font.raster(codepoint)?;
        let built = Arc::new(CachedGlyphInk::from_raster(&raster));
        let mut cache = recover_poisoned_lock(self.ink_runs.lock());
        Some(Arc::clone(cache.entry(key).or_insert(built)))
    }

    #[cfg(test)]
    fn ink_run_cache_len(&self) -> usize {
        recover_poisoned_lock(self.ink_runs.lock()).len()
    }

    /// The advance of `ch` in **device** pixels at `scale`.
    #[must_use]
    pub fn advance(&self, ch: char, scale: f32) -> f32 {
        self.raster
            .advance(ch as u32)
            .unwrap_or(MISSING_ADVANCE)
            .mul_add(scale, 0.0)
    }

    /// The width of a string in device pixels at `scale`, `§`+code pairs counted
    /// as zero-width.
    ///
    /// Identical to [`legacy_width`](Self::legacy_width) — measurement has to
    /// agree with [`draw`](Self::draw), and `draw` decomposes. Vanilla's
    /// `Font.width(String)` decomposes too (`StringDecomposer.iterateFormatted`
    /// into a width sink), so there is no version of this that counts a `§` as a
    /// glyph and is still faithful.
    ///
    /// Both names are kept because a call site that says `legacy_width` is
    /// documenting that its input is `§`-coded, which is worth reading; a call
    /// site that says `width` is making no claim either way and now gets the right
    /// answer regardless. Measuring the raw string over-counted by two characters
    /// per code, which pushed every centred line left of where it drew.
    #[must_use]
    pub fn width(&self, s: &str, scale: f32) -> f32 {
        self.legacy_width(s, scale)
    }

    /// The width of a `§`-coded string in device pixels at `scale`. `§`+code
    /// pairs are zero-width, matching both vanilla and the old fixed path.
    #[must_use]
    pub fn legacy_width(&self, s: &str, scale: f32) -> f32 {
        self.raster.legacy_width(s) * scale
    }

    /// The width of a styled span list in device pixels at `scale`.
    ///
    /// This is the measurement half of [`draw_spans`](Self::draw_spans). It
    /// resolves the span's requested [`FontId`] and each codepoint's custom-font
    /// coverage exactly as drawing does, then derives that selected glyph's
    /// advance. Bold is the only flag that changes an advance; italic shears in
    /// place, and underline/strikethrough/obfuscated leave the pen alone.
    #[must_use]
    pub fn spans_width(&self, spans: &[TextSpan], scale: f32) -> f32 {
        let total: f32 = spans
            .iter()
            .map(|span| {
                let bold = span.style.bold.unwrap_or(false);
                let custom = span.style.font.and_then(|id| self.custom_raster(id));
                span.text
                    .chars()
                    .map(|ch| {
                        let cp = ch as u32;
                        let font = self.select_font(cp, custom.as_deref());
                        let raster = font.raster(cp);
                        glyph_advance(font, cp, raster.as_ref(), bold)
                    })
                    .sum::<f32>()
            })
            .sum();
        total * scale
    }

    /// Draw `s` with its vanilla drop shadow, the string's top-left at `(x, y)`.
    ///
    /// Two passes: the shadow copy first, at 25 % of the colour, then the text.
    /// Drawing the whole string's shadow before any of its glyphs is what keeps
    /// a following glyph's ink on top of the previous glyph's shadow, which is
    /// what vanilla's two-layer batch does. The shadow's offset is **per
    /// glyph** (`Font::shadow_offset`) rather than one constant for the whole
    /// string: 1 logical pixel on both axes for a sheet glyph, 0.5 for a
    /// unihex one (drawn at oversample 2), so a string mixing scripts gets
    /// each glyph's own shadow lag rather than the sheet default applied to
    /// every codepoint.
    /// # This honours `§` codes, and that is not a convenience
    ///
    /// There is no non-decomposing string draw in vanilla to be faithful to.
    /// `Font.drawInBatch(String, …)` goes through
    /// `StringDecomposer.iterateFormatted`, which applies legacy codes at *draw*
    /// time — that single fact is why a plugin server can put `§7` in an item
    /// name and have it colour. A plain pass that emitted `§` and `7` as glyphs
    /// was therefore not "the simple case"; it was the wrong case, and the
    /// surfaces that reached it (item tooltips, the title/subtitle overlay, boss
    /// bar titles, tab-list header and footer, container titles) were exactly the
    /// ones showing raw codes.
    ///
    /// `c`'s alpha is carried through unchanged; its RGB becomes the base colour
    /// that an unstyled run, and `§r`, draw in.
    pub(crate) fn draw(
        &self,
        cs: &mut ColourStream<'_>,
        s: &str,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
    ) {
        self.draw_legacy(cs, s, x, y, scale, [c[0], c[1], c[2]], c[3]);
    }

    /// Draw a `§`-coded string with its drop shadow. Colour codes recolour the
    /// following run, `§r` resets to `base`, and format codes draw real
    /// geometry — see [`resolve_legacy`](Self::resolve_legacy).
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
        let mut glyphs = self.resolve_legacy(s, base);
        bidi_reorder_glyphs(&mut glyphs);
        self.draw_resolved(cs, &glyphs, x, y, scale, alpha, true);
        self.draw_resolved(cs, &glyphs, x, y, scale, alpha, false);
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
        let mut glyphs = self.resolve_spans(spans, base);
        bidi_reorder_glyphs(&mut glyphs);
        self.draw_resolved(cs, &glyphs, x, y, scale, alpha, true);
        self.draw_resolved(cs, &glyphs, x, y, scale, alpha, false);
    }

    /// Decodes a styled span list into [`ResolvedGlyph`]s, in **logical**
    /// (source) order — the non-drawing half of what used to be `spans_run`'s
    /// body. `position` in the old single-pass version counted glyphs across
    /// the whole span list rather than resetting per span
    /// (`Font.java`'s `position == 0` check is about the first glyph of
    /// the *line*), and that is preserved here too: it is
    /// [`draw_resolved`](Self::draw_resolved) that now assigns `position`,
    /// over the final (bidi-reordered) glyph order.
    fn resolve_spans(&self, spans: &[TextSpan], base: [f32; 3]) -> Vec<ResolvedGlyph> {
        let mut out = Vec::with_capacity(spans.iter().map(|s| s.text.len()).sum());
        for span in spans {
            let style = GlyphStyle {
                bold: span.style.bold.unwrap_or(false),
                italic: span.style.italic.unwrap_or(false),
                underline: span.style.underlined.unwrap_or(false),
                strikethrough: span.style.strikethrough.unwrap_or(false),
                obfuscated: span.style.obfuscated.unwrap_or(false),
            };
            let rgb = span.style.color.map_or(base, text_color_rgb);
            let font = span.style.font;
            out.extend(
                span.text
                    .chars()
                    .map(|ch| ResolvedGlyph { ch, style, rgb, font }),
            );
        }
        out
    }

    /// Draw `s` with **no** drop shadow, the string's top-left at `(x, y)`.
    ///
    /// Vanilla's `graphics.text(font, component, x, y, colour, shadow)` takes the
    /// flag as an argument, and the two container labels
    /// pass `false`. Every other text
    /// surface in this crate passes it implicitly by calling
    /// [`draw`](Self::draw), so the shadowless case needs its own name rather
    /// than a bool parameter on the common path.
    ///
    /// "Plain" is about the **shadow**, not about `§` codes — like
    /// [`draw`](Self::draw) this decomposes, because the shadow flag is the only
    /// axis vanilla's own overload varies.
    pub(crate) fn draw_plain(
        &self,
        cs: &mut ColourStream<'_>,
        s: &str,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
    ) {
        let mut glyphs = self.resolve_legacy(s, [c[0], c[1], c[2]]);
        bidi_reorder_glyphs(&mut glyphs);
        self.draw_resolved(cs, &glyphs, x, y, scale, c[3], false);
    }

    /// Decodes a `§`-coded string into [`ResolvedGlyph`]s, in **logical**
    /// (source) order — the non-drawing half of what used to be `legacy_run`'s
    /// body. Format codes carry real geometry: `style` tracks the
    /// five flags across the run exactly as `Font::legacy_width` already
    /// tracks bold for measurement, with the same reset rule
    /// (`lodestone_model::text::apply_legacy_code`) — a colour
    /// code or `§r` clears every flag, not just the one it names.
    ///
    /// `§` control pairs are consumed here and never become a
    /// [`ResolvedGlyph`], which is what keeps them out of
    /// [`bidi_reorder_glyphs`]: the Unicode Bidirectional Algorithm reorders
    /// **visible** text, and vanilla's own `Language.getVisualOrder` likewise
    /// runs over an already-decomposed `FormattedCharSequence`, not a raw
    /// `§`-coded string.
    fn resolve_legacy(&self, s: &str, base: [f32; 3]) -> Vec<ResolvedGlyph> {
        let mut out = Vec::with_capacity(s.len());
        let mut rgb = base;
        let mut style = GlyphStyle::default();
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
            // A `§`-coded run carries no font of its own — see this method's
            // doc — so every glyph it decodes stays on the default font.
            out.push(ResolvedGlyph {
                ch,
                style,
                rgb,
                font: None,
            });
        }
        out
    }

    /// One pass — shadow or main — over an already-resolved, already
    /// bidi-reordered glyph list. The shared drawing half of what used to be
    /// `legacy_run`/`spans_run`'s bodies: both decoders now agree on
    /// [`ResolvedGlyph`], so there is exactly one place glyphs turn into
    /// quads, walked in **visual** order — `position == 0` is therefore the
    /// first glyph drawn left-to-right on screen, matching `Font.java`
    /// even for a right-to-left run.
    fn draw_resolved(
        &self,
        cs: &mut ColourStream<'_>,
        glyphs: &[ResolvedGlyph],
        x: f32,
        y: f32,
        scale: f32,
        alpha: f32,
        shadow: bool,
    ) -> f32 {
        // Text is made from solid, axis-aligned rectangles rather than a
        // filtered texture. Keeping a fractional origin therefore lets a
        // glyph's edge straddle two framebuffer samples; a tooltip or MOTD
        // moving by a fraction of a GUI pixel can make the icon appear to
        // shimmer. Quantise only the line origin here. The pen still advances
        // using the exact provider metrics, so wrapping, centring and the
        // spacing between ordinary glyphs remain unchanged.
        let origin_x = pixel_snap(x);
        let origin_y = pixel_snap(y);
        let mut cursor = origin_x;
        // Only the main pass, never the shadow copy: the shadow's positions
        // are a fixed per-glyph offset from the main pass's own (see the
        // `off` below), so a second, offset-only line per glyph would only
        // add noise, not information, to a layout trace.
        let tracing = !shadow && text_trace_matches(glyphs);
        if tracing {
            eprintln!(
                "lodestone-shell: TEXT_TRACE begin glyphs={} x0={origin_x:.3} y0={origin_y:.3} scale={scale:.3}",
                glyphs.len()
            );
        }
        for (position, g) in glyphs.iter().enumerate() {
            let c = [g.rgb[0], g.rgb[1], g.rgb[2], alpha];
            let c = if shadow { shadow_of(c) } else { c };
            // Which font actually draws this glyph: the span's own
            // `"font"`, if it covers this codepoint, else the default —
            // resolved once per glyph so the shadow offset below and the
            // draw call agree on the same font.
            let custom = g.font.and_then(|id| self.custom_raster(id));
            let font = self.select_font(g.ch as u32, custom.as_deref());
            // Per-glyph shadow offset (`Font::shadow_offset`): 1 px for a
            // sheet glyph, 0.5 for a unihex one, looked up by this glyph's
            // own codepoint rather than assumed uniform across the string —
            // a fixed offset added once, before either pass, was only
            // correct when every glyph shared it.
            let (gx, gy) = if shadow {
                let off = font.font().shadow_offset(g.ch as u32) * scale;
                (cursor + off, origin_y + off)
            } else {
                (cursor, origin_y)
            };
            let pen_before = cursor;
            cursor += self.glyph_styled(cs, g.ch, gx, gy, scale, c, g.style, position == 0, font);
            if tracing {
                self.trace_glyph_line(font, g, pen_before, cursor);
            }
        }
        if tracing {
            eprintln!(
                "lodestone-shell: TEXT_TRACE end total_width={:.3}",
                cursor - origin_x
            );
        }
        cursor - origin_x
    }

    /// One `LODESTONE_TEXT_TRACE` line for a single drawn glyph. `font`,
    /// `pen_before` and `pen_after` are exactly what
    /// [`draw_resolved`](Self::draw_resolved) already computed for this
    /// glyph — nothing here re-derives a number the draw path did not
    /// already produce, except the provider/raster lookups, which are cheap
    /// (bitmap/unihex/space) or already paid for either way by TTF's own
    /// on-demand rasterisation.
    ///
    /// `font=` is which font's cell actually supplied this glyph — compared
    /// by reference against [`VanillaFont::raster`], not by re-deriving
    /// [`select_font`](Self::select_font)'s condition a second time, so a
    /// future change to that condition cannot silently desync the trace from
    /// what was actually drawn. `provider=` and `drawn_w=` are read from
    /// *that* font, so a measure/draw font mismatch (already ruled out
    /// elsewhere, but this is the tool that would have caught it directly)
    /// would show as `drawn_w` disagreeing with `advance` for no fixture
    /// reason.
    fn trace_glyph_line(&self, font: &RasterFont, g: &ResolvedGlyph, pen_before: f32, pen_after: f32) {
        let cp = g.ch as u32;
        let is_default = std::ptr::eq(font, &self.raster);
        let font_label = if is_default {
            DEFAULT_FONT_NAME
        } else {
            g.font.map(FontId::name).unwrap_or(DEFAULT_FONT_NAME)
        };
        let provider = if let Some(b) = font.font().bitmap_glyph(cp) {
            format!("bitmap:{}", b.file)
        } else if font.font().unihex_glyph(cp).is_some() {
            "unihex".to_string()
        } else if let Some(t) = font.font().ttf_glyph(cp) {
            format!("ttf:{}", t.file)
        } else if font.font().contains(cp) {
            "space".to_string()
        } else {
            "missing".to_string()
        };
        let drawn_w = font
            .raster(cp)
            .map(|r| r.cell_width() as f32 * r.texel_size())
            .unwrap_or(0.0);
        eprintln!(
            "lodestone-shell: TEXT_TRACE cp=U+{cp:04X} font={font_label} provider={provider} advance={:.3} pen_x_before={pen_before:.3} pen_x_after={pen_after:.3} drawn_w={drawn_w:.3}",
            pen_after - pen_before
        );
    }

    // There is deliberately no un-styled `glyph` primitive here any more, and no
    // `run` over a plain `&str`. Both existed to serve a "this string cannot
    // carry a `§` code" path that vanilla does not have, and their presence was
    // what let `§7` reach a quad: `glyph_styled` with a default `GlyphStyle` is
    // byte-identical to the old `glyph` (zero bold offset, no obfuscation
    // substitution, no italic shear, `has_effect()` false), so nothing was lost
    // by routing every string through `resolve_legacy`.

    /// Draw one glyph honouring `style`, with the line's top-left at
    /// `(x, y)`. `first` is whether this is the very first glyph
    /// [`draw_resolved`](Self::draw_resolved) draws (`draw_resolved` restarts
    /// its own counter each pass, over the bidi-reordered — i.e. **visual**
    /// — glyph order), matching `Font.java`'s `position == 0` check for
    /// where the underline/strikethrough bar's left edge starts.
    ///
    /// Returns the advance in device pixels, computed from `ch`'s **own**
    /// glyph — even when `style.obfuscated` swaps in a different codepoint's
    /// pixels, see [`obfuscation_pool`](VanillaFont::obfuscation_pool)'s field
    /// docs for why. Effects (underline/strikethrough) and the background
    /// advance vanilla marks per glyph (`Font.java`, `markBackground`) are
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
        font: &RasterFont,
    ) -> f32 {
        let cp = ch as u32;
        let base_ink = self.cached_ink(font, cp);
        let advance = match &base_ink {
            Some(ink) => ink.advance,
            None if font.font().contains(cp) => font.advance(cp).unwrap_or(MISSING_ADVANCE),
            None => MISSING_ADVANCE,
        };
        // `GlyphInfo.getAdvance(boolean)`: `advance + boldOffset` when bold,
        // unchanged otherwise. Vanilla applies this to *every* glyph, drawable
        // or not — a bold space is wider too. The offset is **per glyph**, not a
        // font constant: `UnihexProvider.Glyph.info` overrides `getBoldOffset`
        // to 0.5F because a unihex glyph is drawn at oversample 2, so bold CJK
        // shifts one source texel rather than two.
        let bold_extra = font.font().bold_offset(cp);
        let bold_advance = advance + if style.bold { bold_extra } else { 0.0 };

        // Alpha-zero text must still advance its pen but must not leave empty
        // colour-stream geometry behind (including missing-glyph and effect
        // quads). This also keeps a fully transparent shadow pass a no-op.
        if c[3] == 0.0 {
            return bold_advance * scale;
        }

        if let Some(base_ink) = base_ink {
            // `§k` (and not a space, per `Font.getGlyph`'s `codepoint != 32`
            // guard, which space satisfies here by having no raster at all):
            // substitute a same-width-class glyph's pixels, but keep drawing
            // at `ch`'s own metrics.
            let draw_ink = if style.obfuscated {
                self.obfuscated_ink(advance).unwrap_or(base_ink)
            } else {
                base_ink
            };
            self.draw_ink(cs, &draw_ink, x, y, scale, c, style.italic);
            if style.bold {
                // The second, offset pass that actually makes bold read as
                // bold (`BakedSheetGlyph.renderChar`, `BakedSheetGlyph.java`)
                // — not a font-weight variant, the same glyph redrawn shifted.
                self.draw_ink(
                    cs,
                    &draw_ink,
                    x + bold_extra * scale,
                    y,
                    scale,
                    c,
                    style.italic,
                );
            }
        } else if !font.font().contains(cp) {
            missing_box(cs, x, y, scale, c);
        }

        if style.has_effect() {
            // `Font.java`: `effectX0 = position == 0 ? x - 1.0F : x`.
            let x0 = if first {
                x - font_metrics::EFFECT_LEAD_IN * scale
            } else {
                x
            };
            let x1 = x + bold_advance * scale;
            let thickness = font_metrics::EFFECT_THICKNESS * scale;
            if style.strikethrough {
                // `Font.java`: bar bottom at `y + 4.5F`.
                let bottom = y + font_metrics::STRIKETHROUGH_Y * scale;
                cs.rect(x0, bottom - thickness, x1 - x0, thickness, c);
            }
            if style.underline {
                // `Font.java`: bar bottom at `y + 9.0F`.
                let bottom = y + font_metrics::UNDERLINE_Y * scale;
                cs.rect(x0, bottom - thickness, x1 - x0, thickness, c);
            }
        }

        bold_advance * scale
    }

    /// Emit one cached glyph's ink as merged horizontal runs, with the line's
    /// top-left at `(x, y)`. [`CachedGlyphInk`] has already paid the source
    /// texel scan; this loop only applies the current tint/pose and emits the
    /// same rectangles as the old per-draw scan. Adjacent source-colour runs
    /// are merged again when tint makes their final RGBA equal, preserving the
    /// old geometry as well as the pixels.
    ///
    /// When `italic`, each row is sheared independently: `v` is that row's own
    /// logical-pixel offset from the line's top (matching what
    /// [`CachedGlyphInk::top`] records for the glyph's top edge), and the row
    /// shifts in `x` by `ITALIC_SHEAR - ITALIC_SHEAR_SLOPE * v`
    /// (`BakedSheetGlyph.shearTop`/`shearBottom`,
    /// `BakedSheetGlyph.java`, both `1.0F - 0.25F * v`). Vanilla shears
    /// the whole glyph as one quad with two sheared edges (a continuous linear
    /// interpolation between the top and bottom edge's shear); this evaluates
    /// that same affine function per texel row instead, which is the run-based
    /// renderer's equivalent of "per scanline" once nearest-neighbour sampling
    /// is accounted for — texel rows already are the sampling granularity here.
    fn draw_ink(
        &self,
        cs: &mut ColourStream<'_>,
        ink: &CachedGlyphInk,
        x: f32,
        y: f32,
        scale: f32,
        c: [f32; 4],
        italic: bool,
    ) {
        let texel = ink.texel_size * scale;
        let top = y + ink.top * scale;
        // `GlyphBitmap.getLeft()` / `BakedSheetGlyph`'s `x0 = x + this.left`:
        // zero for a bitmap-sheet or unihex cell (neither overrides the
        // default), but a `ttf` glyph's outline is not generally flush with
        // its advance box, so it carries a real left bearing here.
        let x = x + ink.left * scale;
        let mut i = 0;
        while i < ink.runs.len() {
            let run = ink.runs[i];
            let Some(colour) = modulated_source_rgba(run.source, c) else {
                i += 1;
                continue;
            };
            let mut end = run.end;
            i += 1;
            while let Some(next) = ink.runs.get(i)
                && next.ty == run.ty
                && next.start == end
                && modulated_source_rgba(next.source, c) == Some(colour)
            {
                end = next.end;
                i += 1;
            }
            let shear = if italic {
                let v = ink.top + (run.ty as f32 + 0.5) * ink.texel_size;
                (font_metrics::ITALIC_SHEAR - font_metrics::ITALIC_SHEAR_SLOPE * v) * scale
            } else {
                0.0
            };
            cs.rect(
                x + shear + run.start as f32 * texel,
                top + run.ty as f32 * texel,
                (end - run.start) as f32 * texel,
                texel,
                colour,
            );
        }
    }

    /// Picks a cached `§k` replacement from [`obfuscation_pool`](VanillaFont::obfuscation_pool),
    /// keyed by `ceil(original_advance)` — vanilla's own width class
    /// (`FontSet.java`, `Mth.ceil(glyph.info().getAdvance(false))`), and
    /// advances the free-running picker once. `None` only when this font has
    /// no drawable glyph at all of that exact rounded width.
    fn obfuscated_ink(&self, original_advance: f32) -> Option<Arc<CachedGlyphInk>> {
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
        self.cached_ink(&self.raster, pool[idx] as u32)
    }
}

/// Snap a text line's pixel-space origin to the nearest GUI pixel. Text is
/// emitted as binary coverage rectangles, so a fractional line origin would
/// make those rectangles cross sample boundaries and visibly shimmer while a
/// moving tooltip/MOTD changes its position. Keeping this at the line origin
/// (rather than rounding each pen advance) preserves the font's exact layout.
#[inline]
fn pixel_snap(value: f32) -> f32 {
    value.round()
}

/// The logical advance that [`VanillaFont::glyph_styled`] and
/// [`VanillaFont::spans_width`] must agree on. `raster` is the same lookup the
/// draw path already made, so a drawable glyph takes its raster's metrics; a
/// declared-but-non-drawable glyph (such as a space) still takes its provider
/// advance; an uncovered codepoint takes the default missing-glyph advance.
fn glyph_advance(
    font: &RasterFont,
    codepoint: u32,
    raster: Option<&GlyphRaster<'_>>,
    bold: bool,
) -> f32 {
    let advance = match raster {
        Some(raster) => raster.advance(),
        None if font.font().contains(codepoint) => font.advance(codepoint).unwrap_or(MISSING_ADVANCE),
        None => MISSING_ADVANCE,
    };
    advance
        + if bold {
            font.font().bold_offset(codepoint)
        } else {
            0.0
        }
}

/// `LODESTONE_TEXT_TRACE`'s target: unset disables the whole feature, `"all"`
/// (case-insensitive) fires on every styled draw, anything else is a
/// substring the drawn text must contain. Checked once per process and
/// cached, since [`VanillaFont::draw_resolved`] runs at least once per frame
/// for every visible piece of text.
///
/// This is the *sequence* diagnostic CLAUDE.md's own audit habit calls for
/// once every single-glyph metric has been checked and the symptom is still
/// unexplained: `LODESTONE_FONT_METRICS`/`LODESTONE_FONT_TRACE` in
/// `lodestone_assets::font` can each only speak about one codepoint's own
/// numbers in isolation. Neither can show a codepoint that never reaches
/// this function at all -- dropped between the packet and the [`TextSpan`]
/// it should have become -- because there is nothing to print for a glyph
/// that was never in the list. A gap that vanishes at this layer despite
/// every provider computing a healthy advance is exactly that case.
fn text_trace_target() -> Option<&'static str> {
    static TARGET: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TARGET
        .get_or_init(|| std::env::var("LODESTONE_TEXT_TRACE").ok())
        .as_deref()
}

/// Whether [`VanillaFont::draw_resolved`] should trace this particular
/// resolved glyph list, per [`text_trace_target`]'s rule.
fn text_trace_matches(glyphs: &[ResolvedGlyph]) -> bool {
    let Some(target) = text_trace_target() else {
        return false;
    };
    if target.eq_ignore_ascii_case("all") {
        return true;
    }
    if target.is_empty() {
        return false;
    }
    glyphs.iter().map(|g| g.ch).collect::<String>().contains(target)
}

/// The final colour a raster texel contributes after the component/text pass
/// colour has modulated its native source RGBA. Transparent source or pass
/// alpha deliberately produces no quad at all rather than six transparent
/// vertices in the HUD stream.
fn modulated_source_rgba(source: [f32; 4], tint: [f32; 4]) -> Option<[f32; 4]> {
    let rgba = [
        source[0] * tint[0],
        source[1] * tint[1],
        source[2] * tint[2],
        source[3] * tint[3],
    ];
    (rgba[3] != 0.0).then_some(rgba)
}

/// Keeps the custom-font cache fail-open after a loader panic poisoned its
/// mutex. Resource-pack contents are untrusted, so a later nameplate must be
/// able to fall back instead of panicking again.
fn recover_poisoned_lock<T>(result: Result<T, PoisonError<T>>) -> T {
    result.unwrap_or_else(PoisonError::into_inner)
}

/// Groups every codepoint this font can actually draw pixels for by
/// `ceil(advance)`, mirroring `FontSet.glyphsByWidth`
/// restricted to codepoints
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
///
/// # Why the asset-object store is pushed on top
///
/// The jar alone is **not enough for the font**, and this is the one place in the
/// shell where that matters. `client.jar` ships a 29-byte
/// `assets/minecraft/font/include/unifont.json` **stub whose `providers` array is
/// empty**; the real 3,993-byte file — the one that declares the `unihex`
/// provider and its `size_overrides` — lives only in the launcher's asset-object
/// store, and so does `font/unifont.zip`. Loading `minecraft:default` from the
/// jar alone therefore resolves the stub, contributes zero unihex glyphs, logs a
/// perfectly healthy "loaded the vanilla default font", and draws the
/// missing-glyph box for all 112,018 codepoints the three bitmap sheets do not
/// cover. That is the entire "squares instead of CJK" symptom, and no amount of
/// rasteriser work fixes it without this push.
///
/// [`ResourceManager::push`] adds at the **highest** priority, which is the
/// direction `asset_objects`' own rule demands: *for any name present in both,
/// prefer the object store.* Only 8 of the 5,057 index objects share a name with
/// a jar entry, and the store's copy is the real asset in all 8.
///
/// A store that will not open is not an error here — it is the pre-unihex state,
/// which still renders every Latin/European codepoint from the jar. Watch
/// [`Font::unihex_count`](lodestone_assets::font::Font::unihex_count) in the load
/// log to tell the two apart; the codepoint total alone cannot, because it moves
/// whenever any provider does.
fn jar_manager() -> Option<ResourceManager> {
    // `open_vanilla_pack_stack`, not `vanilla_manager`: the latter is the raw
    // jar with no pack layering (it exists for GPU gates comparing rendered
    // pixels against source art), and a font resolved against it can never
    // see a resource pack's own `font/*.json` — see that function's own doc.
    // This is also the manager [`load_custom_font`] uses, so a server- or
    // locally-selected pack's custom font is discoverable the same way the
    // default font already is.
    let mut manager = crate::resources::open_vanilla_pack_stack()?;
    // The browser has no object store: `platform::assets::Bundle` carries the jar
    // and the blocks report only, so a wasm session keeps bitmap-only coverage
    // rather than paying a 1.5 MB fetch for `unifont.zip` on a bundle that is
    // already over its size ceiling.
    #[cfg(not(target_arch = "wasm32"))]
    match crate::asset_objects::discover_store_root()
        .and_then(|root| crate::asset_objects::AssetObjectStore::open(&root))
    {
        Ok(store) => {
            tracing::debug!(
                target: "assets",
                objects = store.len(),
                "asset-object store pushed above client.jar for the font (unifont)"
            );
            manager.push(Box::new(store));
        }
        Err(e) => tracing::info!(
            target: "assets",
            "no asset-object store for the font, so unihex/CJK coverage is off: {e}"
        ),
    }
    Some(manager)
}

/// Loads one non-default font by its `"namespace:path"` id, from the same
/// resource-pack-aware manager [`jar_manager`] builds for `minecraft:default`
/// — so a server- or locally-selected pack's `assets/<ns>/font/<name>.json`
/// is reachable exactly the way its `assets/<ns>/textures/**` already is.
///
/// Fail-open, like every pack loader in this crate: the pack is untrusted
/// input (a malformed provider list, a truncated PNG, a `.zip` that does not
/// open), and any parse failure here must degrade to the default font rather
/// than panic the client. `None` is also what a genuinely absent font id
/// (the server never shipped it, or the player has no matching pack
/// selected) produces — the two cases are indistinguishable from here, and
/// [`VanillaFont::select_font`] treats them identically: draw from the
/// default font instead.
fn load_custom_font(name: &str) -> Option<Arc<RasterFont>> {
    let manager = jar_manager()?;
    let id: ResourceLocation = name.parse().ok()?;
    match FontLoader::new(&manager).load_raster(&id, &FontOptions::none()) {
        Ok(raster) => {
            tracing::info!(
                target: "assets",
                font = name,
                codepoints = raster.font().codepoint_count(),
                unihex = raster.unihex_count(),
                "loaded a resource-pack font"
            );
            Some(Arc::new(raster))
        }
        Err(e) => {
            tracing::warn!(target: "assets", "load resource-pack font {name}: {e}");
            None
        }
    }
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

/// Does the **shell's own** font manager resolve unihex glyphs?
///
/// `lodestone-assets` proves the rasteriser; nothing there proves that
/// [`jar_manager`] — the function the shipped client actually calls — stacks the
/// asset-object store above the jar. Without that push a fully correct rasteriser
/// draws squares, with a healthy log line, which is precisely the island shape
/// this repo keeps paying for. So this gate goes through the real discovery path
/// and nothing else.
///
/// ```text
/// cargo test -p lodestone-shell --lib -- --ignored --nocapture unihex_wiring
/// ```
#[cfg(test)]
mod unihex_wiring {
    use super::*;

    /// The shell's own manager must supply unihex glyphs, and specifically must
    /// beat the jar's empty `font/include/unifont.json` stub.
    ///
    /// The two counts are the discriminating pair: a jar-only manager reports
    /// `unihex = 0` and `codepoints = 2414`, and both numbers come from the files
    /// rather than from us — 2,414 is the bitmap sheets plus the `space`
    /// provider, 114,432 is `unifont.zip`'s own entry count.
    #[test]
    #[ignore = "needs client.jar plus the unifont.json/unifont.zip asset objects"]
    fn the_shells_own_font_manager_resolves_unihex_glyphs() {
        let manager = jar_manager().expect(
            "no vanilla pack found; set LODESTONE_ASSETS or populate .cache/mc/<ver>/ \
             — do NOT skip, a silent pass here asserts nothing",
        );
        let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
        let font = FontLoader::new(&manager)
            .load(&id, &FontOptions::none())
            .expect("minecraft:default loads");
        eprintln!(
            "  shell font: codepoints={} unihex={}",
            font.codepoint_count(),
            font.unihex_count()
        );
        assert!(
            font.unihex_count() > 0,
            "the shell's font manager resolved NO unihex glyphs. Either the \
             asset-object store did not open, or font/unifont.zip is not on disk. \
             Fetch it with: cargo run -p xtask -- fetch-assets --version 26.2"
        );
        // Exact, not just non-zero: `unifont.zip` holds 114,432 entries and is a
        // superset of the 2,414 the sheets and the space provider supply.
        assert_eq!(font.codepoint_count(), 114_432);
        assert_eq!(font.unihex_count(), 114_432 - 2_414);

        // The codepoint from the report, and the one that discriminates: U+2713
        // is unihex-only, U+2714 is in nonlatin_european.png and must keep the
        // sheet's advance of 7.0 rather than unihex's 8.5.
        let raster = FontLoader::new(&manager)
            .load_raster(&id, &FontOptions::none())
            .expect("the font rasters");
        let cjk = raster
            .raster(0x4E2D)
            .expect("中 must have drawable pixels, not a missing-glyph box");
        assert!((cjk.advance() - 9.0).abs() < 1e-6, "got {}", cjk.advance());
        assert!((cjk.texel_size() - 0.5).abs() < 1e-6);
        assert_eq!(raster.advance(0x2713), Some(4.5));
        assert_eq!(
            raster.advance(0x2714),
            Some(7.0),
            "the bitmap sheet must still win a codepoint both providers supply"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::text::TextStyle;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::mpsc;
    use std::time::Duration;

    /// The default font's staleness compare: [`refresh_shared_font`] re-resolves
    /// exactly when the caller's stamp is behind the live pack generation, and
    /// does nothing when it is not.
    ///
    /// **This covers the compare, not the reload.** A jar-less run resolves
    /// `None` for every generation, so the *font* cannot be observed changing
    /// here; what is asserted is the decision — which is precisely what was
    /// missing when this was a `OnceLock` and the answer was "never re-resolve",
    /// and it is the half a gate can reach with no `client.jar` present.
    ///
    /// The stamp is moved by hand rather than by calling `set_selected_packs`:
    /// that is process-wide state shared with every other test in this binary,
    /// and a gate that mutates it would be changing the pack stack out from
    /// under whatever else is running.
    #[test]
    fn a_stale_stamp_re_resolves_the_default_font_and_a_current_one_does_not() {
        // Explicit generations, never `pack_generation()`: that counter is
        // process-wide and a concurrently running pack test moves it, which
        // made the first version of this gate fail for a reason that had
        // nothing to do with the code under test.
        let mut font = None;
        let mut stamp = 7u64;

        assert!(
            refresh_shared_font_to(&mut font, &mut stamp, 8),
            "a stamp behind the current generation must re-resolve"
        );
        assert_eq!(
            stamp, 8,
            "the stamp must become what was actually resolved against, or every \
             later frame re-resolves"
        );

        // The control: called again with nothing changed, it must not. Without
        // this the assertion above is satisfied by a function that always
        // reloads — a different bug with the same green.
        assert!(
            !refresh_shared_font_to(&mut font, &mut stamp, 8),
            "an up-to-date stamp must not re-resolve"
        );
        assert_eq!(stamp, 8, "and must leave the stamp alone");

        // A generation that went *backwards* still counts as a change. The
        // counter never decreases in production, so this is only asserting
        // that the compare is inequality rather than ordering — which is what
        // `pack_generation`'s own doc promises its value means.
        assert!(
            refresh_shared_font_to(&mut font, &mut stamp, 3),
            "any different generation re-resolves; the compare is not ordered"
        );
    }

    /// A tiny raster font is enough to exercise the custom-font cache without
    /// requiring a local `client.jar`: this test is about cache invalidation,
    /// not bitmap drawing.
    fn space_raster(ch: char) -> RasterFont {
        let mut source = lodestone_assets::MemorySource::new("custom-font-cache");
        source.insert(
            "assets/minecraft/font/default.json",
            format!(r#"{{"providers":[{{"type":"space","advances":{{"{ch}":4}}}}]}}"#)
                .into_bytes(),
        );
        let manager = ResourceManager::new(vec![Box::new(source)]);
        let id: ResourceLocation = "minecraft:default".parse().expect("valid fixture id");
        FontLoader::new(&manager)
            .load_raster(&id, &FontOptions::none())
            .expect("space-only fixture font loads")
    }

    fn font_with_custom_cache() -> VanillaFont {
        let raster = space_raster(' ');
        font_with_raster(raster)
    }

    fn font_with_raster(raster: RasterFont) -> VanillaFont {
        VanillaFont {
            obfuscation_pool: build_obfuscation_pool(&raster),
            raster,
            obfuscation_rng: AtomicU64::new(0x9E37_79B9_7F4A_7C15),
            ink_runs: Mutex::new(HashMap::new()),
            custom: Mutex::new(CustomFontCache::default()),
        }
    }

    fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .expect("PNG header")
            .write_image_data(rgba)
            .expect("PNG data");
        bytes
    }

    /// A one-row bitmap font fixture with explicitly controlled source texels.
    fn bitmap_raster(chars: &str, cell_width: u32, rgba: &[u8]) -> RasterFont {
        let width = cell_width * chars.chars().count() as u32;
        let png = encode_png(width, 1, rgba);
        let mut source = lodestone_assets::MemorySource::new("font-rgba-fixture");
        source.insert("assets/minecraft/textures/font/t.png", png);
        source.insert(
            "assets/minecraft/font/default.json",
            format!(
                r#"{{"providers":[{{"type":"bitmap","file":"minecraft:font/t.png","ascent":1,"height":1,"chars":["{chars}"]}}]}}"#
            )
            .into_bytes(),
        );
        let manager = ResourceManager::new(vec![Box::new(source)]);
        let id: ResourceLocation = "minecraft:default".parse().expect("valid fixture id");
        FontLoader::new(&manager)
            .load_raster(&id, &FontOptions::none())
            .expect("bitmap fixture font loads")
    }

    #[test]
    fn repeated_glyph_draw_reuses_cached_ink_runs_without_changing_geometry() {
        let raster = bitmap_raster("A", 2, &[255, 255, 255, 255, 255, 255, 255, 255]);
        let font = font_with_raster(raster);
        let draw = || {
            let mut verts = Vec::new();
            font.draw(
                &mut ColourStream {
                    verts: &mut verts,
                    w: 100.0,
                    h: 100.0,
                },
                "A",
                5.0,
                7.0,
                1.0,
                [1.0; 4],
            );
            verts
        };

        assert_eq!(font.ink_run_cache_len(), 0);
        let first = draw();
        assert_eq!(
            font.ink_run_cache_len(),
            1,
            "shadow and main passes for one codepoint must share one cached raster walk"
        );
        let second = draw();
        assert_eq!(
            font.ink_run_cache_len(),
            1,
            "drawing the same codepoint on a later frame must not rescan its texels"
        );
        assert_eq!(second, first, "caching must be geometry-transparent");
    }

    #[test]
    fn text_origin_is_pixel_stable_across_subpixel_motion() {
        let font = font_with_raster(bitmap_raster("A", 1, &[255, 255, 255, 255]));
        let draw = |x: f32| {
            let mut verts = Vec::new();
            font.draw_plain(
                &mut ColourStream {
                    verts: &mut verts,
                    w: 100.0,
                    h: 100.0,
                },
                "A",
                x,
                7.0,
                1.0,
                [1.0; 4],
            );
            verts
        };

        let first = draw(5.1);
        let second = draw(5.4);
        assert_eq!(
            first, second,
            "moving a text origin within one pixel must not move a binary glyph's geometry"
        );
        assert_ne!(
            first,
            draw(5.6),
            "text must still advance to the next pixel once the origin crosses its midpoint"
        );
    }

    #[test]
    fn custom_font_generation_change_invalidates_cached_ink_runs() {
        let font = font_with_raster(bitmap_raster("A", 1, &[255, 255, 255, 255]));
        let id = FontId::intern("test:generation-ink-cache");
        let first = Arc::new(bitmap_raster("A", 1, &[255, 0, 0, 255]));
        let first = font
            .custom_raster_for_generation(id, 11, || Some(first))
            .expect("first custom font");
        assert!(font.cached_ink(&first, 'A' as u32).is_some());
        assert_eq!(font.ink_run_cache_len(), 1);

        let second = Arc::new(bitmap_raster("A", 1, &[0, 255, 0, 255]));
        assert!(
            font.custom_raster_for_generation(id, 12, || Some(second))
                .is_some()
        );
        assert_eq!(
            font.ink_run_cache_len(),
            0,
            "a new pack generation must not retain runs keyed by freed custom-font addresses"
        );
    }

    fn span(text: &str, font: Option<FontId>) -> TextSpan {
        TextSpan {
            text: text.to_owned(),
            style: TextStyle {
                font,
                ..TextStyle::default()
            },
        }
    }

    #[test]
    fn bitmap_rgba_is_tinted_alpha_modulated_and_run_split() {
        // Adjacent texels differ only in source colour, so a binary-ink run
        // merger would wrongly emit one quad. Both have partial alpha.
        let font = font_with_raster(bitmap_raster(
            "A",
            2,
            &[
                128, 128, 128, 128, // grey, 50% source alpha
                64, 128, 192, 64,   // a distinct translucent source colour
            ],
        ));
        let mut verts = Vec::new();
        {
            let mut cs = ColourStream {
                verts: &mut verts,
                w: 100.0,
                h: 100.0,
            };
            font.draw_plain(&mut cs, "A", 10.0, 10.0, 1.0, [0.5, 0.25, 1.0, 0.5]);
        }
        // `ColourStream::rect` writes six vertices with six floats each.
        assert_eq!(verts.len(), 72, "different final RGBA texels need two runs");
        assert_eq!(
            &verts[2..6],
            &[
                128.0 / 255.0 * 0.5,
                128.0 / 255.0 * 0.25,
                128.0 / 255.0,
                128.0 / 255.0 * 0.5,
            ]
        );
        assert_eq!(
            &verts[38..42],
            &[
                64.0 / 255.0 * 0.5,
                128.0 / 255.0 * 0.25,
                192.0 / 255.0,
                64.0 / 255.0 * 0.5,
            ]
        );

        let mut transparent = Vec::new();
        {
            let mut cs = ColourStream {
                verts: &mut transparent,
                w: 100.0,
                h: 100.0,
            };
            font.draw_plain(&mut cs, "A", 10.0, 10.0, 1.0, [1.0, 1.0, 1.0, 0.0]);
        }
        assert!(transparent.is_empty(), "alpha-zero text must emit no vertices");
    }

    #[test]
    fn spans_width_uses_custom_metrics_then_default_fallback_and_matches_pen() {
        // Default A/B advance 2 each; custom A advances 5 and deliberately
        // omits B. A structured span must therefore measure/draw A=5, B=2.
        let font = font_with_raster(bitmap_raster(
            "AB",
            1,
            &[
                255, 255, 255, 255, // default A
                255, 255, 255, 255, // default B
            ],
        ));
        let custom_id = FontId::intern("test:wide-icons");
        let custom = Arc::new(bitmap_raster(
            "A",
            4,
            &[
                255, 255, 255, 255,
                255, 255, 255, 255,
                255, 255, 255, 255,
                255, 255, 255, 255,
            ],
        ));
        let generation = crate::resources::pack_generation();
        {
            let mut cache = font.custom.lock().expect("custom cache lock");
            cache.generation = Some(generation);
            cache.entries.insert(custom_id, Some(custom));
        }
        let spans = [span("AB", Some(custom_id))];

        assert_eq!(font.spans_width(&spans, 1.0), 7.0);
        let mut glyphs = font.resolve_spans(&spans, [1.0, 1.0, 1.0]);
        bidi_reorder_glyphs(&mut glyphs);
        let mut verts = Vec::new();
        let pen = {
            let mut cs = ColourStream {
                verts: &mut verts,
                w: 100.0,
                h: 100.0,
            };
            font.draw_resolved(&mut cs, &glyphs, 20.0, 20.0, 1.0, 1.0, false)
        };
        assert_eq!(pen, font.spans_width(&spans, 1.0));
    }

    /// A missing custom font is cached during one pack generation so a chat
    /// nameplate does not attempt a load every frame. Once the pack stack
    /// changes, that negative result must be retried: the same `VanillaFont`
    /// outlives an asynchronously installed server pack.
    #[test]
    fn custom_font_cache_retries_a_missing_font_after_pack_generation_changes() {
        let font = font_with_custom_cache();
        let id = FontId::intern("nameplates:default");
        let available = std::cell::Cell::new(false);
        let attempts = std::cell::Cell::new(0);
        let load = || {
            attempts.set(attempts.get() + 1);
            available.get().then(|| Arc::new(space_raster('X')))
        };

        assert!(
            font.custom_raster_for_generation(id, 41, load).is_none(),
            "the pre-install lookup must fail"
        );
        assert_eq!(attempts.get(), 1);

        available.set(true);
        assert!(
            font.custom_raster_for_generation(id, 41, || {
                attempts.set(attempts.get() + 1);
                Some(Arc::new(space_raster('X')))
            })
            .is_none(),
            "an unchanged generation must retain the negative cache entry"
        );
        assert_eq!(attempts.get(), 1, "an unchanged generation must not reload every frame");

        let loaded = font
            .custom_raster_for_generation(id, 42, || {
                attempts.set(attempts.get() + 1);
                Some(Arc::new(space_raster('X')))
            })
            .expect("a changed pack generation must retry and use the newly available font");
        assert!(loaded.font().contains('X' as u32));
        assert_eq!(attempts.get(), 2);
    }

    /// The first lookup owns the cache mutex until it records its result. A
    /// second thread asking for the same font/generation must consume that
    /// result rather than loading (and warning about) the same missing font.
    #[test]
    fn concurrent_custom_font_lookups_share_one_first_load() {
        let font = Arc::new(font_with_custom_cache());
        let id = FontId::intern("nameplates:shift_1");
        let attempts = Arc::new(AtomicUsize::new(0));
        let (first_started_tx, first_started_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_font = Arc::clone(&font);
        let first_attempts = Arc::clone(&attempts);
        let first = std::thread::Builder::new()
            .name("font-cache-first-loader".to_owned())
            .spawn(move || {
                first_font.custom_raster_for_generation(id, 88, || {
                    first_attempts.fetch_add(1, AtomicOrdering::SeqCst);
                    first_started_tx.send(()).expect("test is listening");
                    release_first_rx.recv().expect("test releases first loader");
                    None
                })
            })
            .expect("native test worker starts");
        first_started_rx.recv().expect("first loader started");

        let (second_call_tx, second_call_rx) = mpsc::channel();
        let (second_loader_tx, second_loader_rx) = mpsc::channel();
        let second_font = Arc::clone(&font);
        let second_attempts = Arc::clone(&attempts);
        let second = std::thread::Builder::new()
            .name("font-cache-second-loader".to_owned())
            .spawn(move || {
                second_call_tx.send(()).expect("test is listening");
                second_font.custom_raster_for_generation(id, 88, || {
                    second_attempts.fetch_add(1, AtomicOrdering::SeqCst);
                    second_loader_tx.send(()).expect("test is listening");
                    None
                })
            })
            .expect("native test worker starts");
        second_call_rx.recv().expect("second lookup started");

        // Before the fix the second call reaches its loader while the first
        // one waits above. With the mutex held, it cannot get past the cache
        // lookup until the first call stores its negative result.
        let duplicate_load = second_loader_rx.recv_timeout(Duration::from_millis(100));
        release_first_tx.send(()).expect("first loader is waiting");
        assert!(first.join().expect("first worker did not panic").is_none());
        assert!(second.join().expect("second worker did not panic").is_none());
        assert!(
            duplicate_load.is_err(),
            "the second same-generation lookup invoked its loader before the first cached its result"
        );
        assert_eq!(attempts.load(AtomicOrdering::SeqCst), 1);
    }

    /// The custom-font cache follows this module's existing fail-open mutex
    /// policy: a panic while holding it must not turn later nameplates into a
    /// second panic.
    #[test]
    fn custom_font_cache_recovers_after_its_mutex_is_poisoned() {
        // The default Cranelift test backend aborts on a deliberately panicking
        // spawned thread (documented in Cargo.toml), so construct the exact
        // poisoned lock result instead. Production calls this helper with the
        // cache mutex's `lock()` result.
        let recovered = recover_poisoned_lock(Err(PoisonError::new(CustomFontCache::default())));
        assert_eq!(recovered.generation, None);
        assert!(recovered.entries.is_empty());
    }

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

    /// Pure right-to-left text (no LTR characters at all) auto-detects an RTL
    /// paragraph and reverses as one run: the leftmost glyph on screen
    /// (visual position 0) is the **last** logical character. Values are the
    /// `unicode-bidi` crate's own output for this input, not a hand-derived
    /// guess — `unicode-bidi` is an outside implementation of UAX #9, so this
    /// is the "captured bytes from an independent source" species of
    /// expected value, not `decode(encode(x)) == x`.
    #[test]
    fn bidi_reverses_a_pure_rtl_run() {
        // ' Hebrew alef, bet, gimel.
        let order = bidi_visual_order("\u{5d0}\u{5d1}\u{5d2}");
        assert_eq!(order, [2, 1, 0]);
    }

    /// A discriminating input: LTR text immediately followed by RTL text,
    /// with **no separator** between the two runs (a bare space is itself
    /// direction-neutral and would leave open whether its own placement,
    /// rather than the run boundary, produced the answer). The LTR run keeps
    /// its logical order and the RTL run reverses in place after it — this
    /// is the case a same-direction-only fixture (all-LTR or all-RTL) cannot
    /// exercise, matching CLAUDE.md's own point about pairwise-distinct
    /// fixtures.
    #[test]
    fn bidi_reorders_a_mixed_ltr_then_rtl_run() {
        let order = bidi_visual_order("abc\u{5d0}\u{5d1}\u{5d2}");
        assert_eq!(order, [0, 1, 2, 5, 4, 3]);
    }

    /// [`bidi_reorder_glyphs`] must leave pure-ASCII glyph lists untouched
    /// (the fast path this module's doc promises), and must actually permute
    /// a mixed-script list rather than merely running without panicking.
    #[test]
    fn bidi_reorder_glyphs_permutes_only_when_needed() {
        fn glyph(ch: char) -> ResolvedGlyph {
            ResolvedGlyph {
                ch,
                style: GlyphStyle::default(),
                rgb: [1.0, 1.0, 1.0],
                font: None,
            }
        }

        let mut ascii: Vec<ResolvedGlyph> = "abc".chars().map(glyph).collect();
        bidi_reorder_glyphs(&mut ascii);
        assert_eq!(
            ascii.iter().map(|g| g.ch).collect::<String>(),
            "abc",
            "pure ASCII must not be reordered"
        );

        let mut mixed: Vec<ResolvedGlyph> = "ab\u{5d0}\u{5d1}".chars().map(glyph).collect();
        bidi_reorder_glyphs(&mut mixed);
        assert_eq!(
            mixed.iter().map(|g| g.ch).collect::<String>(),
            "ab\u{5d1}\u{5d0}",
            "the trailing Hebrew run must reverse in place after the LTR run"
        );
    }

    /// Same fixture shape as [`bitmap_raster`], but with an explicit declared
    /// `height` independent of the source image's row height — the only way
    /// to get `pixel_scale != 1.0`. Every existing bitmap fixture in this
    /// module (and in `lodestone-assets/tests/font.rs`'s own draw-adjacent
    /// corpus) fixes `height` equal to the image's row height, which pins
    /// `pixel_scale` at exactly `1.0`: the one value where "the draw path
    /// multiplies by `pixel_scale`" and "it doesn't" are indistinguishable.
    fn bitmap_raster_scaled(
        chars: &str,
        cell: u32,
        declared_height: u32,
        rgba: &[u8],
    ) -> RasterFont {
        let width = cell * chars.chars().count() as u32;
        let png = encode_png(width, cell, rgba);
        let mut source = lodestone_assets::MemorySource::new("font-scaled-fixture");
        source.insert("assets/minecraft/textures/font/t.png", png);
        source.insert(
            "assets/minecraft/font/default.json",
            format!(
                r#"{{"providers":[{{"type":"bitmap","file":"minecraft:font/t.png","ascent":1,"height":{declared_height},"chars":["{chars}"]}}]}}"#
            )
            .into_bytes(),
        );
        let manager = ResourceManager::new(vec![Box::new(source)]);
        let id: ResourceLocation = "minecraft:default".parse().expect("valid fixture id");
        FontLoader::new(&manager)
            .load_raster(&id, &FontOptions::none())
            .expect("scaled bitmap fixture font loads")
    }

    /// The corpus-wide coincidence this repo's evidence standards warn about:
    /// every other bitmap fixture here declares `height` equal to its
    /// image's row height, which pins `pixel_scale` at `1.0` — the one value
    /// at which a `draw_ink` that forgot to multiply the cell's own texel
    /// walk by `pixel_scale` (while still applying it correctly to
    /// `advance`, which is exercised elsewhere) would be indistinguishable
    /// from a correct one. A server-provided background-panel font is
    /// exactly the shape that breaks the coincidence: a physically large
    /// sheet cell scaled down to a modest logical size.
    ///
    /// Two fully-opaque, back-to-back glyphs at a non-integer `pixel_scale`
    /// (0.625) are the discriminating input: if the cell's drawn extent used
    /// the raw physical cell width instead of `cell_width * pixel_scale`,
    /// the second glyph's ink would start well *before* the first glyph's
    /// own (wrongly-large) drawn extent ends — the "background block
    /// swallowing the next glyph" the bug report describes.
    #[test]
    fn bitmap_draw_extent_respects_pixel_scale_not_just_advance() {
        // 32x32 physical cell, declared height 20 -> pixel_scale = 20/32 =
        // 0.625, deliberately non-integer. 'A' and 'B' are both fully opaque
        // squares (a stand-in for an opaque background-panel sprite).
        let opaque_cell = vec![255u8; 32 * 32 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let raster = bitmap_raster_scaled("AB", 32, 20, &rgba);
        let font = font_with_raster(raster);

        // Vanilla's own formula (`BitmapProvider.Definition.load`): actual=32
        // (fully opaque, rightmost column has ink), pixel_scale=0.625 ->
        // `(int)(0.5 + 32*0.625) + 1` = `(int)(20.5) + 1` = 21.
        let a_advance = font.raster.advance('A' as u32).expect("A resolves");
        assert_eq!(
            a_advance, 21.0,
            "advance itself must already reflect pixel_scale"
        );

        let mut verts = Vec::new();
        {
            let mut cs = ColourStream {
                verts: &mut verts,
                w: 400.0,
                h: 400.0,
            };
            font.draw(&mut cs, "AB", 50.0, 50.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        }
        // Isolate the main pass (drop the shadow copy, which draws at 0.25
        // brightness) by colour, the same trick this module's other
        // pixel-geometry tests use.
        let xs: Vec<f32> = verts
            .chunks_exact(6)
            .filter(|v| {
                (v[2] - 1.0).abs() < 1e-4 && (v[3] - 1.0).abs() < 1e-4 && (v[4] - 1.0).abs() < 1e-4
            })
            .map(|v| (v[0] + 1.0) * 400.0 * 0.5)
            .collect();
        assert!(!xs.is_empty(), "the opaque cells must draw real ink");
        let max_x = xs.iter().copied().fold(f32::MIN, f32::max);
        let min_x = xs.iter().copied().fold(f32::MAX, f32::min);
        // Correct: 'A' draws local x in [0, 32*0.625] = [0, 20] -> screen
        // [50, 70]. 'B's pen starts at 50 + 21 = 71 and draws local [0, 20]
        // of its own -> screen [71, 91]. Total extent: [50, 91].
        //
        // The wrong hypothesis (pixel_scale dropped from the cell's texel
        // walk, raw 32px cell drawn instead of the scaled 20px one): 'A'
        // would draw screen [50, 82] and 'B' screen [71, 103] — the two
        // glyphs' own ink would overlap from x=71 to x=82, and the total
        // extent would read 103, not 91. The two hypotheses differ by 12px,
        // well outside float slop.
        assert!(
            (min_x - 50.0).abs() < 0.01,
            "got min_x={min_x}, want 50.0"
        );
        assert!(
            (max_x - 91.0).abs() < 0.01,
            "got max_x={max_x}, want 91.0 (32px raw cell would give 103.0 -- \
             draw_ink must scale the cell's texel walk by pixel_scale, not \
             just the advance)"
        );
    }

    /// A hermetic reproduction of the exact composition a "background panel"
    /// resource-pack font uses: a **custom** font (selected via `Style.font`,
    /// not `minecraft:default`) declaring both bitmap panel glyphs *and* a
    /// `space` provider entry for a positive-advance gap character between
    /// them -- e.g. a Unicode figure/en/thin-space codepoint remapped to a
    /// specific pixel width, the standard vanilla technique for inserting a
    /// precise gap that no bitmap cell's own ink-derived advance could
    /// produce. If a `space` glyph's advance were ever dropped on this path
    /// (measured but not drawn-through, or resolved against the wrong font,
    /// or substituted with `MISSING_ADVANCE`), the gap would vanish and the
    /// second panel's ink would start where the first panel's own advance
    /// ends -- immediately after the first panel, with no room for the
    /// intended gap. This is "the gap between them is completely missing"
    /// reproduced end to end, through the same `draw_spans`/`glyph_styled`
    /// path production text takes, not a synthetic call into `Font::advance`
    /// alone.
    #[test]
    fn a_positive_space_glyph_between_two_custom_bitmap_panels_opens_a_real_gap() {
        // Two 8px-wide fully-opaque "panel" cells at pixel_scale 1 (this test
        // is about the space provider, not pixel_scale -- that is already
        // covered by `bitmap_draw_extent_respects_pixel_scale_not_just_advance`).
        let opaque_cell = vec![255u8; 8 * 8 * 4];
        let mut rgba = Vec::with_capacity(opaque_cell.len() * 2);
        rgba.extend_from_slice(&opaque_cell);
        rgba.extend_from_slice(&opaque_cell);
        let png = encode_png(16, 8, &rgba);

        let mut source = lodestone_assets::MemorySource::new("panel-gap-fixture");
        source.insert("assets/nameplates/textures/font/panels.png", png);
        // U+2007 FIGURE SPACE remapped to a real 40px advance -- the
        // "positive spacer" mechanism; the panels are ordinary ASCII stand-ins
        // 'A'/'B' since the mechanism under test does not depend on which
        // codepoints the panels themselves occupy.
        source.insert(
            "assets/nameplates/font/default.json",
            br#"{"providers":[
                {"type":"space","advances":{"\u2007":40}},
                {"type":"bitmap","file":"nameplates:font/panels.png","ascent":7,"height":8,"chars":["AB"]}
            ]}"#
                .to_vec(),
        );
        let manager = ResourceManager::new(vec![Box::new(source)]);
        let custom_id_loc: ResourceLocation = "nameplates:default".parse().expect("valid id");
        let custom_raster = FontLoader::new(&manager)
            .load_raster(&custom_id_loc, &FontOptions::none())
            .expect("panel+space fixture font loads");

        // A base font that has nothing at all -- the default font must never
        // be consulted for a codepoint the custom font itself defines.
        let font = font_with_raster(bitmap_raster("X", 1, &[0, 0, 0, 0]));
        let custom_id = FontId::intern("nameplates:default");
        {
            let mut cache = font.custom.lock().expect("custom cache lock");
            cache.generation = Some(crate::resources::pack_generation());
            cache.entries.insert(custom_id, Some(std::sync::Arc::new(custom_raster)));
        }

        let spans = [span("A\u{2007}B", Some(custom_id))];

        // Measurement side: advance(A) + 40 (the space) + advance(B).
        // advance(A)/(B) = (0.5 + 8*1.0) as i32 + 1 = 8 + 1 = 9 each (fully
        // opaque 8px cell, pixel_scale 1).
        let measured = font.spans_width(&spans, 1.0);
        assert_eq!(
            measured, 58.0,
            "spans_width must include the space glyph's own 40px advance, got {measured}"
        );

        // Draw side: the panels must not overlap. Isolate ink vertices by
        // colour (white, [1,1,1,1]) exactly like this module's other
        // pixel-geometry tests.
        let mut verts = Vec::new();
        {
            let mut cs = ColourStream {
                verts: &mut verts,
                w: 400.0,
                h: 400.0,
            };
            font.draw_spans(&mut cs, &spans, 50.0, 50.0, 1.0, [1.0, 1.0, 1.0], 1.0);
        }
        let xs: Vec<f32> = verts
            .chunks_exact(6)
            .filter(|v| {
                (v[2] - 1.0).abs() < 1e-4 && (v[3] - 1.0).abs() < 1e-4 && (v[4] - 1.0).abs() < 1e-4
            })
            .map(|v| (v[0] + 1.0) * 400.0 * 0.5)
            .collect();
        assert!(!xs.is_empty(), "the opaque panels must draw real ink");
        let max_x = xs.iter().copied().fold(f32::MIN, f32::max);
        let min_x = xs.iter().copied().fold(f32::MAX, f32::min);
        // 'A' draws local [0, 8] -> screen [50, 58]. The space glyph itself
        // draws nothing but advances the pen by 40 -> 'B' starts at pen
        // 50 + 9 + 40 = 99, drawing local [0, 8] -> screen [99, 107].
        assert!((min_x - 50.0).abs() < 0.01, "got min_x={min_x}, want 50.0");
        assert!(
            (max_x - 107.0).abs() < 0.01,
            "got max_x={max_x}, want 107.0 (a dropped/zeroed space advance would put \
             this at 58.0 -- 'B' drawn immediately after 'A' with no gap at all -- \
             or at 67.0 for a MISSING_ADVANCE substitution)"
        );
    }

    /// The coordinator's specific hypothesis: **two different fonts
    /// disagreeing** about which one supplies a codepoint, where measuring
    /// (`spans_width`) and drawing (`draw_spans`) each independently call
    /// [`VanillaFont::select_font`] -- if they ever resolved to different
    /// fonts for the *same* glyph, the pen would advance by one font's
    /// number while the ink came from the other's cell, which is exactly
    /// "renders correctly, overlaps, gap completely missing": a large
    /// low-advance glyph's ink bleeding into the next glyph's territory
    /// while the *measured* string width still (correctly) reflects the
    /// small advance.
    ///
    /// The default font declares codepoint 'C' as an 8px-wide fully opaque
    /// cell (advance 9 -- standing in for the real report's unihex glyph at
    /// advance 9.000 for U+753C). The custom font *also* declares 'C', as a
    /// 1px-wide fully opaque cell (advance 2 -- standing in for the real
    /// report's `backgrounds/b1.png` at advance 2). Both `spans_width` and
    /// the drawn ink must agree on the **custom** font's numbers throughout,
    /// never mixing the two.
    #[test]
    fn measuring_and_drawing_a_custom_font_glyph_resolve_the_same_font_not_the_default() {
        // Default font: 'C' is an 8px fully-opaque cell -> actual=8 ->
        // advance=(0.5+8) as i32 + 1 = 9.
        let default_raster = bitmap_raster("C", 8, &vec![255u8; 8 * 4]);
        let font = font_with_raster(default_raster);
        assert_eq!(
            font.raster.advance('C' as u32),
            Some(9.0),
            "fixture sanity: the default font's own 'C' must be advance 9"
        );

        // Custom font: 'C' is a 1px fully-opaque cell -> actual=1 ->
        // advance=(0.5+1) as i32 + 1 = 2.
        let custom_raster = bitmap_raster("C", 1, &[255, 255, 255, 255]);
        assert_eq!(
            custom_raster.advance('C' as u32),
            Some(2.0),
            "fixture sanity: the custom font's own 'C' must be advance 2"
        );
        let custom_id = FontId::intern("nameplates:default");
        {
            let mut cache = font.custom.lock().expect("custom cache lock");
            cache.generation = Some(crate::resources::pack_generation());
            cache
                .entries
                .insert(custom_id, Some(std::sync::Arc::new(custom_raster)));
        }

        let spans = [span("C", Some(custom_id))];

        // Measurement must be the custom font's 2, never the default's 9.
        let measured = font.spans_width(&spans, 1.0);
        assert_eq!(
            measured, 2.0,
            "spans_width must resolve 'C' through the custom font (2), not the \
             default font (9) -- got {measured}"
        );

        // Draw must also be the custom font's 1px cell, never the default's
        // 8px one -- checked by the drawn ink's own extent, not just the pen
        // advance, so a "correct advance, wrong ink" split would still be
        // caught.
        let mut verts = Vec::new();
        {
            let mut cs = ColourStream {
                verts: &mut verts,
                w: 400.0,
                h: 400.0,
            };
            font.draw_spans(&mut cs, &spans, 50.0, 50.0, 1.0, [1.0, 1.0, 1.0], 1.0);
        }
        let xs: Vec<f32> = verts
            .chunks_exact(6)
            .filter(|v| {
                (v[2] - 1.0).abs() < 1e-4 && (v[3] - 1.0).abs() < 1e-4 && (v[4] - 1.0).abs() < 1e-4
            })
            .map(|v| (v[0] + 1.0) * 400.0 * 0.5)
            .collect();
        assert!(!xs.is_empty(), "'C' must draw real ink");
        let max_x = xs.iter().copied().fold(f32::MIN, f32::max);
        let min_x = xs.iter().copied().fold(f32::MAX, f32::min);
        assert!((min_x - 50.0).abs() < 0.01, "got min_x={min_x}, want 50.0");
        assert!(
            (max_x - 51.0).abs() < 0.01,
            "got max_x={max_x}, want 51.0 (the custom font's own 1px cell) -- 58.0 \
             would mean the ink was drawn from the *default* font's 8px cell while \
             the pen advanced by the custom font's 2px, exactly the measure/draw \
             font-mismatch hypothesis"
        );
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

    /// **Bold**: `BakedSheetGlyph.renderChar`
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
    /// (`BakedSheetGlyph.shearTop`/`shearBottom`, `BakedSheetGlyph.java`).
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

    /// **Underline / strikethrough**: `Font.java` draws a 1px bar
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
    /// (`Font.java`, `STRIKETHROUGH_Y` vs `UNDERLINE_Y`) — the two
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

    /// A colour code (not just `§r`) resets bold —
    /// `lodestone_model::text::apply_legacy_code`: "a legacy colour code resets
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

    /// **Obfuscated**: `Font.getGlyph` swaps in a random
    /// same-width-class glyph every time it is asked, from a `RandomSource`
    /// that is never reseeded — so two draws of the *same*
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

    /// Space is never obfuscated (`Font.java`, `codepoint != 32`) — an
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

    /// Vanilla's sixteen, hand-transcribed from `TextColor.java` (26.2).
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
                font: None,
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
