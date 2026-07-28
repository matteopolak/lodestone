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
//! * **Bold / italic / obfuscated are not drawn.** The metrics for them exist
//!   (`Font::advance_bold`, `metrics::ITALIC_SHEAR`); the draw side consumes
//!   `§l`/`§o` as zero-width state and ignores them, exactly as the old path did.
//!   Adding them is a change *here*, not in `lodestone-assets`.
//! * **Only bitmap providers rasterise.** `unihex` (CJK) and `ttf` parse but
//!   contribute no glyphs, so those codepoints render as the missing-glyph box.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use lodestone_assets::font::{
    FontLoader, FontOptions, MISSING_ADVANCE, RasterFont, metrics as font_metrics,
};
use lodestone_assets::{ResourceLocation, ResourceManager, ResourceSource, ZipSource};

use super::item_icon::ColourStream;

/// Vanilla's missing-glyph box: a 5×8 hollow rectangle with a 1 px edge, advance
/// `5 + 1`. Mirrors `SpecialGlyphs.MISSING` in the 26.2 client.
const MISSING_W: u32 = 5;
/// Height of the missing-glyph box, in logical pixels.
const MISSING_H: u32 = 8;

/// The vanilla `minecraft:default` font, loaded with pixels, ready to draw.
#[derive(Debug)]
pub struct VanillaFont {
    raster: RasterFont,
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
                Some(Arc::new(VanillaFont { raster }))
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
        Ok(VanillaFont {
            raster: FontLoader::new(manager).load_raster(&id, &FontOptions::none())?,
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
    /// following run, `§r` resets to `base`, and format codes are consumed
    /// zero-width (no bold/italic geometry yet — see the module docs).
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

    /// One unshadowed pass over a plain string.
    fn run(&self, cs: &mut ColourStream<'_>, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let mut cursor = x;
        for ch in s.chars() {
            cursor += self.glyph(cs, ch, cursor, y, scale, c);
        }
    }

    /// One unshadowed pass over a `§`-coded string. `shadow` scales every run's
    /// colour, so the two passes walk identical geometry.
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
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                match chars.next() {
                    Some(code) => {
                        if let Some(v) = super::legacy_rgb(code) {
                            rgb = v;
                        } else if code.eq_ignore_ascii_case(&'r') {
                            rgb = base;
                        }
                    }
                    None => break,
                }
                continue;
            }
            let c = [rgb[0], rgb[1], rgb[2], alpha];
            let c = if shadow { shadow_of(c) } else { c };
            cursor += self.glyph(cs, ch, cursor, y, scale, c);
        }
    }

    /// Draw one glyph with the line's top-left at `(x, y)` and return its
    /// advance in device pixels.
    ///
    /// Ink is emitted as horizontal runs rather than per texel: an 8×8 cell is
    /// at most 8 quads instead of up to 64, and the merged quad is pixel-identical
    /// because every texel in a run shares one colour.
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
                cs.rect(
                    x + start as f32 * texel,
                    top + ty as f32 * texel,
                    (tx - start) as f32 * texel,
                    texel,
                    c,
                );
            }
        }
        r.advance() * scale
    }
}

/// Open `client.jar` from a discovered vanilla pack root as a [`ResourceManager`].
///
/// This mirrors [`crate::resources`]'s pack discovery rather than calling it,
/// because `resources::vanilla_manager` is `#[cfg(test)]` — it exists only for
/// unit tests inside the lib, and production code cannot reach it. Dropping that
/// attribute and calling it here is the right end state; it is a one-line change
/// in a file this module does not own. Until then the discovery rule is
/// duplicated *exactly*: `LODESTONE_ASSETS` if set and complete, else the
/// highest-sorting `.cache/mc/<version>` under any ancestor of the working
/// directory that holds both `client.jar` and `generated/reports/blocks.json`.
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
