use super::*;

/// The RGB of one of the sixteen legacy `§` colour codes (`0`..=`9`, `a`..=`f`),
/// or `None` for a format/reset code. These are the standard Minecraft chat
/// foreground colours; the shell paints them locally, which is a rendering
/// concern (how to colour a run), not protocol knowledge.
///
/// This used to hold its own transcription of the sixteen hex constants. It now
/// delegates to [`TextColor::rgb`], which is the same table sourced from
/// vanilla's own text-color declarations — one copy, so the two cannot drift, and so a
/// `TextColor`-carrying draw path (`vanilla_font::draw_spans`) and this
/// `§`-carrying one are guaranteed to agree on what "gold" means.
pub(super) fn legacy_rgb(code: char) -> Option<[f32; 3]> {
    TextColor::from_legacy_code(code).map(vanilla_font::text_color_rgb)
}

pub(super) struct Builder<'a> {
    pub(super) w: f32,
    pub(super) h: f32,
    pub(super) verts: Vec<f32>,
    pub(super) sprite_verts: Vec<f32>,
    pub(super) item_verts: Vec<f32>,
    /// The enchantment-glint copies of `item_verts`; see [`IconSink::glint`].
    pub(super) glint_verts: Vec<f32>,
    pub(super) model_verts: Vec<ModelVertex>,
    /// Special-renderer (block-entity) icons; see [`HudGeometry::special`].
    pub(super) special: Vec<SpecialIconDraw>,
    /// See [`HudGeometry::crosshair`].
    pub(super) crosshair: Option<std::ops::Range<u32>>,
    pub(super) gui: Option<&'a GuiAtlas>,
    pub(super) items: Option<&'a ItemAtlas>,
    /// The baked model set, for items whose inventory icon is a 3-D mini-block
    /// rather than a flat sprite. `None` on jar-less / demo runs, where those
    /// slots stay empty wells exactly as before.
    pub(super) models: Option<&'a BlockModels>,
    /// The vanilla proportional font. `None` on jar-less / demo runs and in every
    /// pure `HudGeometry::build*` call, where text falls back to the fixed-advance
    /// 5×7 debug font. Measurement and drawing read the *same* field, so a layout
    /// can never be computed against a font other than the one that draws.
    pub(super) font: Option<&'a VanillaFont>,
}

impl<'a> Builder<'a> {
    pub(super) fn new(
        w: f32,
        h: f32,
        gui: Option<&'a GuiAtlas>,
        items: Option<&'a ItemAtlas>,
        models: Option<&'a BlockModels>,
        font: Option<&'a VanillaFont>,
    ) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            sprite_verts: Vec::new(),
            item_verts: Vec::new(),
            glint_verts: Vec::new(),
            model_verts: Vec::new(),
            special: Vec::new(),
            crosshair: None,
            gui,
            items,
            models,
            font,
        }
    }

    /// Pixel width of `s` at `scale` in whichever font [`Builder::text`] will
    /// draw with. Every centring and right-alignment site must use this.
    pub(super) fn text_width(&self, s: &str, scale: f32) -> f32 {
        measure_text(self.font, s, scale)
    }

    /// Pixel width of a `§`-coded string at `scale`, codes counted as zero-width.
    pub(super) fn legacy_width(&self, s: &str, scale: f32) -> f32 {
        match self.font {
            Some(f) => f.legacy_width(s, scale),
            None => item_icon::text_w(&strip_legacy(s), scale),
        }
    }

    /// Pixel width of a styled span list at `scale` — the measurement partner of
    /// [`text_spans`](Self::text_spans), so a right-aligned styled cell lands on
    /// the same pen positions the draw will use.
    pub(super) fn spans_width(&self, spans: &[TextSpan], scale: f32) -> f32 {
        match self.font {
            Some(f) => f.spans_width(spans, scale),
            None => spans
                .iter()
                .map(|s| item_icon::text_w(&s.text, scale))
                .sum(),
        }
    }

    /// Greedy word-wrap of a legacy `§`-coded line into rows that each fit
    /// `max_width_px` at `scale`, measured with whichever metrics
    /// [`Builder::legacy_width`] reports — real vanilla proportional glyph
    /// advances when a [`VanillaFont`] is attached, the fixed 5×7 advance
    /// otherwise. Mirrors vanilla's own reflow in shape (the gui message's split lines,
    /// invoked from the chat component's add message to display queue,
    /// vanilla's own chat-component rendering): break on a space when the next word
    /// would overflow, and hard-break a single word that alone exceeds the
    /// width so nothing can escape the box. A `§` colour/format code seen
    /// before a break is carried onto the continuation line, because a code
    /// resets formatting to just itself
    /// (`Text::from_legacy`'s legacy semantics) — tracking only the single
    /// most recent one is therefore sufficient to keep the colour continuous
    /// across the wrap.
    ///
    /// Never returns an empty vector: an empty `s` yields one empty row, and a
    /// `max_width_px <= 0.0` (or a line that already fits) is returned as a
    /// single unwrapped row rather than looping forever trying to shrink it.
    ///
    /// A thin wrapper over [`wrap_legacy_with`] bound to this `Builder`'s own
    /// [`Builder::legacy_width`] — see that function for the algorithm. Kept
    /// separate so the wrap logic itself can be unit-tested against an
    /// injected width table (real proportional advances) without needing a
    /// GPU, an atlas, or a loaded jar.
    pub(super) fn wrap_legacy(&self, s: &str, max_width_px: f32, scale: f32) -> Vec<String> {
        wrap_legacy_with(|t| self.legacy_width(t, scale), s, max_width_px)
    }

    /// The [`TextSpan`] sibling of [`Builder::wrap_legacy`]: a thin wrapper
    /// over [`wrap_spans_with`] bound to this `Builder`'s own
    /// [`Builder::spans_width`], kept separate for the same reason —
    /// unit-testing the wrap *decision* against an injected width table with
    /// no GPU, atlas, or jar involved.
    pub(super) fn wrap_spans(&self, spans: &[TextSpan], max_width_px: f32, scale: f32) -> Vec<Vec<TextSpan>> {
        wrap_spans_with(|s| self.spans_width(s, scale), spans, max_width_px)
    }

    /// Draw one hotbar slot's icon into the `size`×`size` rect at `(x, y)`: the
    /// icon itself, its durability bar, and its stack count.
    ///
    /// Delegates to the shared [`item_icon::draw_item_icon`], which is the one
    /// implementation the container screen also uses; see that module for how
    /// the two icon kinds reach two different streams.
    pub(super) fn item_icon(&mut self, slot: &ItemIcon, x: f32, y: f32, size: f32) {
        let assets = IconAssets {
            items: self.items,
            models: self.models,
        };
        let (w, h) = (self.w, self.h);
        let mut sink = IconSink {
            colour: ColourStream {
                verts: &mut self.verts,
                w,
                h,
            },
            sprite: &mut self.item_verts,
            model: &mut self.model_verts,
            special: &mut self.special,
            glint: &mut self.glint_verts,
        };
        item_icon::draw_item_icon(&mut sink, &assets, (w, h), slot, x, y, size, self.font);
    }

    /// As [`Builder::item_icon`], but the icon squashes/stretches through
    /// vanilla's pickup "pop" animation first — `pop` is
    /// `hud::anim::HotbarPop`'s `5.0 → 0.0` amount, `0.0` (idle) drawing
    /// pixel-identically to [`Builder::item_icon`] (see
    /// [`item_icon::draw_item_icon_popped`] for the vanilla citations).
    pub(super) fn item_icon_popped(&mut self, slot: &ItemIcon, x: f32, y: f32, size: f32, pop: f32) {
        let assets = IconAssets {
            items: self.items,
            models: self.models,
        };
        let (w, h) = (self.w, self.h);
        let mut sink = IconSink {
            colour: ColourStream {
                verts: &mut self.verts,
                w,
                h,
            },
            sprite: &mut self.item_verts,
            model: &mut self.model_verts,
            special: &mut self.special,
            glint: &mut self.glint_verts,
        };
        item_icon::draw_item_icon_popped(&mut sink, &assets, (w, h), slot, x, y, size, self.font, pop);
    }

    /// Emit a GUI sprite scaled into the pixel rect `(x, y, w, h)`, tinted by
    /// `c`. A no-op when no atlas is attached or the id is unknown, so callers
    /// need not branch.
    pub(super) fn sprite(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        for q in self.gui_geometry(id, x, y, w, h) {
            self.push_sprite_quad(q, c);
        }
    }

    /// The raw textured quads for a sprite, for callers that post-process them
    /// (for example cropping the XP progress bar to its filled fraction). Empty
    /// when no atlas is attached or the id is unknown.
    pub(super) fn gui_geometry(&self, id: &str, x: f32, y: f32, w: f32, h: f32) -> Vec<GuiSpriteQuad> {
        match self.gui {
            Some(gui) => gui.geometry(id, x, y, w, h),
            None => Vec::new(),
        }
    }

    /// Push one textured quad (two triangles) from an absolute-pixel destination
    /// rect and its atlas UVs, tinted by `c`.
    pub(super) fn push_sprite_quad(&mut self, q: GuiSpriteQuad, c: [f32; 4]) {
        item_icon::push_sprite_quad(&mut self.sprite_verts, self.w, self.h, q, c);
    }

    /// Emit a pixel-space rectangle as two triangles in NDC.
    pub(super) fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.colour().rect(x, y, w, h, c);
    }

    /// A handle onto the colour stream, for the shared pixel-space primitives.
    pub(super) fn colour(&mut self) -> ColourStream<'_> {
        ColourStream {
            w: self.w,
            h: self.h,
            verts: &mut self.verts,
        }
    }

    /// Emit a row of 10 pips representing a `0..=20` gauge (health/food). A pip
    /// lights once any of its two units is present; empty pips render as a dark
    /// slot so the gauge width reads at a glance.
    pub(super) fn pips(&mut self, units: f32, x: f32, y: f32, pip: f32, gap: f32, full: [f32; 4]) {
        let empty = [0.12, 0.12, 0.14, 0.8];
        for i in 0..10 {
            let lit = units > (i as f32) * 2.0;
            let col = if lit { full } else { empty };
            self.rect_px(x + i as f32 * (pip + gap), y, pip, pip, col);
        }
    }

    /// Emit a string starting at pixel `(x, y)` (top-left of first glyph).
    ///
    /// With a [`VanillaFont`] attached this is vanilla text: proportional
    /// advances, real `ascii.png` glyphs and the 1 px drop shadow. Without one it
    /// is the fixed-advance 5×7 debug font, unshadowed, exactly as before.
    pub(super) fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let (w, h) = (self.w, self.h);
                f.draw(
                    &mut ColourStream {
                        verts: &mut self.verts,
                        w,
                        h,
                    },
                    s,
                    x,
                    y,
                    scale,
                    c,
                );
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// Emit a string with **no** drop shadow, the string's top-left at
    /// `(x, y)`. The contextual bar's extract experience level
    /// (vanilla's hud rendering/vanilla's contextual-bar rendering) builds the XP level number's
    /// outline out of four unshadowed offset copies plus one unshadowed centre
    /// copy — passing `shadow = false` to `graphics.text` every time — so a
    /// caller reproducing that outline must use this, not [`text`](Self::text):
    /// `text` always adds vanilla's *automatic* 1px shadow on top of whatever
    /// is drawn, which would layer a second, unwanted shadow under the
    /// hand-rolled one. The fixed-advance debug font (no [`VanillaFont`]
    /// attached) was already unshadowed, so that branch is unchanged.
    pub(super) fn text_plain(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let (w, h) = (self.w, self.h);
                f.draw_plain(
                    &mut ColourStream {
                        verts: &mut self.verts,
                        w,
                        h,
                    },
                    s,
                    x,
                    y,
                    scale,
                    c,
                );
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// Draw a single glyph with its top-left at `(x, y)`. Space and unknown
    /// handling match [`font::glyph_rows`]; blanks emit no quads.
    pub(super) fn glyph(&mut self, ch: char, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        self.colour().glyph(ch, x, y, scale, c);
    }

    /// Emit a string carrying legacy `§` colour/format codes as coloured runs.
    /// Colour codes (`§0`..=`§f`) recolour the following text; `§r` resets to
    /// `base`. With a [`VanillaFont`] attached, the five format codes
    /// (`§k`/`l`/`m`/`n`/`o`) draw real bold/italic/underline/strikethrough/
    /// obfuscated geometry (see `hud/vanilla_font.rs`'s module docs). Without
    /// one — the fixed-advance debug font — they are consumed but not styled,
    /// since that font has no styled glyph variants. Each code pair is
    /// **zero-width** either
    /// way (beyond whatever geometry the style itself adds, e.g. bold's `+1`
    /// advance), matching vanilla's "`§` codes are 2 chars / 0 width", so
    /// coloured and plain text of the same visible length line up exactly.
    /// `alpha` scales every run for the fade-out.
    pub(super) fn text_legacy(&mut self, s: &str, x: f32, y: f32, scale: f32, base: [f32; 3], alpha: f32) {
        if let Some(f) = self.font {
            let (w, h) = (self.w, self.h);
            f.draw_legacy(
                &mut ColourStream {
                    verts: &mut self.verts,
                    w,
                    h,
                },
                s,
                x,
                y,
                scale,
                base,
                alpha,
            );
            return;
        }
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        let mut rgb = base;
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                // A colour/format code: consume the following selector, adjust
                // state, and advance the cursor by nothing.
                match chars.next() {
                    Some(code) => {
                        if let Some(c) = legacy_rgb(code) {
                            rgb = c;
                        } else if code.eq_ignore_ascii_case(&'r') {
                            rgb = base;
                        }
                        // Format codes (k/l/m/n/o) and unknowns: swallowed.
                    }
                    None => break,
                }
                continue;
            }
            self.glyph(ch, cursor, y, scale, [rgb[0], rgb[1], rgb[2], alpha]);
            cursor += advance;
        }
    }

    /// Emit a list of styled spans as coloured runs — the structured twin of
    /// [`text_legacy`](Self::text_legacy).
    ///
    /// Prefer this over `text_legacy` for anything that starts life as a
    /// [`Text`](lodestone_model::text::Text). Flattening to a `§` string first
    /// is lossy in a way that is invisible at the call site: a
    /// [`TextColor::Rgb`] has no legacy code, so `Text::to_legacy_string`
    /// silently drops it and the run renders in `base`. Spans keep the
    /// `TextColor`, so the hex colours modern servers actually send survive to
    /// the quad.
    ///
    /// A span with no colour of its own draws in `base`; `alpha` scales every
    /// run, for fades.
    pub(super) fn text_spans(
        &mut self,
        spans: &[TextSpan],
        x: f32,
        y: f32,
        scale: f32,
        base: [f32; 3],
        alpha: f32,
    ) {
        if let Some(f) = self.font {
            let (w, h) = (self.w, self.h);
            f.draw_spans(
                &mut ColourStream {
                    verts: &mut self.verts,
                    w,
                    h,
                },
                spans,
                x,
                y,
                scale,
                base,
                alpha,
            );
            return;
        }
        // Jar-less debug font: fixed advance, colour only. Mirrors
        // `text_legacy`'s fallback, which likewise cannot style a glyph.
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        for span in spans {
            let rgb = span.style.color.map_or(base, vanilla_font::text_color_rgb);
            for ch in span.text.chars() {
                self.glyph(ch, cursor, y, scale, [rgb[0], rgb[1], rgb[2], alpha]);
                cursor += advance;
            }
        }
    }
}
