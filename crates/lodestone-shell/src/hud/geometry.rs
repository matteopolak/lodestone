use super::*;

/// Builds the HUD vertex stream (positions in NDC, RGBA per vertex) for a given
/// viewport. Pure, so it is unit-testable without a GPU.
#[derive(Debug)]
pub struct HudGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex.
    pub verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per textured GUI-sprite vertex. Empty
    /// unless a [`GuiAtlas`] was supplied to [`HudGeometry::build_with_gui`].
    pub sprite_verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per textured **item**-sprite vertex, drawn
    /// from the separate [`ItemAtlas`] texture. Empty unless an item atlas and
    /// [`HudFrame::hotbar_items`] were both supplied.
    pub item_verts: Vec<f32>,
    /// The **enchantment-glint** copies of [`item_verts`](Self::item_verts):
    /// one quad per flat sprite layer of every enchanted stack, same rect and
    /// same atlas UVs, drawn on its own pipeline over the icon.
    /// Empty when nothing on screen is enchanted.
    pub glint_verts: Vec<f32>,
    /// The 3-D **block-item** icons: baked model geometry already posed into GUI
    /// pixel space on the CPU, in the wide [`ModelVertex`] format the shared
    /// [`ModelPipeline`] consumes. Non-indexed (six vertices per quad, expanded
    /// from the mesh's indices) to match the other two streams' `draw(0..n)`.
    ///
    /// Pre-multiplying the pose here is what collapses the whole hotbar to **one
    /// buffer and one draw**: the GUI path has to emit vertices anyway, so
    /// transforming them costs nothing over uploading them untransformed and
    /// paying a per-slot uniform + draw call. Empty unless a [`BlockModels`] was
    /// supplied and at least one slot holds an item with 3-D geometry.
    pub model_verts: Vec<ModelVertex>,
    /// The **special-renderer** icons (chests and the rest of the ex-
    /// `builtin/entity` family): not vertices, but which baked block-entity mesh
    /// and sheet to draw and the GUI-space placement to draw it under. The meshes
    /// are resident from attach time, so a slot costs a handful of matrices.
    ///
    /// `pub(crate)` rather than `pub` because [`SpecialIconDraw`] is: this is an
    /// internal hand-off to [`IconRenderer::upload`], not part of the geometry a
    /// caller inspects, and nothing outside the crate constructs a
    /// [`HudGeometry`].
    pub(crate) special: Vec<SpecialIconDraw>,
    /// The crosshair's vertices within [`verts`](Self::verts), drawn with the
    /// colour-inverting blend so the mark reads against any background.
    pub crosshair: Option<std::ops::Range<u32>>,
}

impl HudGeometry {
    /// Number of vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }

    /// Number of textured GUI-sprite vertices.
    #[must_use]
    pub fn sprite_vertex_count(&self) -> usize {
        self.sprite_verts.len() / SPRITE_FLOATS_PER_VERTEX
    }

    /// Build the whole HUD for `width`×`height` pixels from a [`HudFrame`],
    /// drawing the survival vitals (hotbar, XP, hearts, hunger) as procedural
    /// quads. This is the jar-less / headless path.
    #[must_use]
    pub fn build(frame: &HudFrame, width: u32, height: u32) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            None,
            None,
            None,
            None,
            HudAnim::NONE,
            true,
        )
    }

    /// Like [`build`](Self::build), but with vanilla text: proportional advances
    /// and the drop shadow, from the real `ascii.png`. Everything else is
    /// identical.
    ///
    /// Kept separate from [`build`](Self::build) deliberately — `build` must stay
    /// jar-free and byte-deterministic, because it is what the geometry unit
    /// tests and the jar-less fallback path use.
    #[must_use]
    pub fn build_with_font(
        frame: &HudFrame,
        width: u32,
        height: u32,
        font: &VanillaFont,
    ) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            None,
            None,
            None,
            Some(font),
            HudAnim::NONE,
            true,
        )
    }

    /// Like [`build`](Self::build), but draws the survival vitals from the real
    /// vanilla GUI atlas (hearts, hunger, XP bar, hotbar frame + selection)
    /// instead of procedural quads. Everything else (debug text, chat, sidebar,
    /// crosshair, …) is identical and still emitted to the colour stream.
    #[must_use]
    #[cfg(test)]
    pub fn build_with_gui(frame: &HudFrame, width: u32, height: u32, gui: &GuiAtlas) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            Some(gui),
            None,
            None,
            None,
            HudAnim::NONE,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_inner(
        frame: &HudFrame,
        width: u32,
        height: u32,
        gui_scale: u32,
        gui: Option<&GuiAtlas>,
        items: Option<&ItemAtlas>,
        models: Option<&BlockModels>,
        font: Option<&VanillaFont>,
        anim: HudAnim,
        include_debug: bool,
    ) -> Self {
        // `width`/`height` are the **physical** framebuffer, straight from
        // `winit::inner_size()` — already DPI-scaled, exactly what
        // `crate::menu::render::logical_canvas` expects. Dividing it down to the
        // logical canvas here, and laying every fixed pixel constant below into
        // that smaller space, is the whole fix for "the HUD draws at half size on
        // a Retina display": the constants themselves never change, only the
        // canvas they are laid into. Reuses the exact helper `menu/render.rs`
        // already uses for the menu screens, rather than a second scale
        // computation that could disagree with it. `gui_scale` is the resolved
        // `Options.gui_scale`, threaded in by the caller — `build`/`build_with_font`/
        // `build_with_gui` above pass `AUTO_GUI_SCALE` explicitly since they are
        // the jar-less/headless/test paths, which have no persisted option to
        // read; `render_with_item_models` (the real windowed path) passes the
        // live value from `menu::nav::MenuNav::gui_scale()` via `app.rs`.
        let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
        let mut b = Builder::new(w, h, gui, items, models, font);

        let margin = HUD_MARGIN;
        let glyph_h = font::GLYPH_H as f32;

        // The F3 overlay, in vanilla's **two columns**: player and world on the
        // left, engine internals on the right, each line sitting on its own
        // translucent plate. `DebugStats::left_lines` records why each block of
        // lines sits in the column it does.
        //
        // The right column is right-aligned at `w - margin - text_width(line)`,
        // which is vanilla's `guiWidth() - 2 - font.width(line)`
        // (`DebugScreenOverlay.extractLines`), so a long line grows leftwards
        // instead of off the screen. The width has to come from `b.text_width`,
        // the same measure the draw itself uses — a restated constant would
        // misalign the moment the vanilla font is or is not loaded.
        //
        // **Right-alignment is not a fit guarantee**, which is the defect that
        // built [`debug_overlay`]: it places a line correctly and says nothing
        // about whether the line is narrower than the canvas. The frame
        // profile's `world_encode_submit` line is several times the logical
        // canvas width at any real GUI scale, so most of it — including the
        // `world.submit` reading it exists to report — rendered off the edge.
        // Every row now goes through `debug_overlay::layout_columns`, which
        // measures and breaks before it positions.
        //
        // **Vanilla's own metrics, not an ad-hoc HUD-wide one.** The overlay
        // used to draw at double vanilla's size, which is exactly the mistake
        // the XP level number's own comment records one screen over:
        // this function already draws in the `gui_scale`-divided logical canvas,
        // so a ×2 on the text made it twice vanilla's size relative to
        // everything around it. `DebugScreenOverlay` draws at scale 1 with
        // `MARGIN_LEFT == MARGIN_RIGHT == MARGIN_TOP == 2` and a line height of
        // `9` — see [`DEBUG_MARGIN`] and [`DEBUG_LINE_H`].
        if include_debug {
            draw_debug_overlay(&mut b, frame);
        }

        // The F3+Shift profiler pie chart — independent of `frame.show_debug`
        // above only in the sense that its own gate lives on the data, not
        // the draw: `profiler_chart` is `None` whenever the debug overlay is
        // closed or the chart itself is toggled off
        // (`app::redraw`'s own `if self.show_debug && self.show_profiler_chart`
        // gate), so there is nothing to double-guard here.
        if let Some(chart) = &frame.stats.profiler_chart {
            draw_profiler_chart(&mut b, chart);
        }

        // Chat, bottom-left: an optional input line at the very bottom, with the
        // received log stacked above it. Received lines carry legacy `§` colour
        // codes (rendered as coloured runs) and fade out with age like vanilla
        // once the box is closed; while it's open, the full history stays lit.
        //
        // `opts` is [`ChatDisplayOptions`] — see that type for the vanilla
        // field each knob reproduces. `chat_pose_scale` is vanilla's own
        // chat-scale option alone, exactly as vanilla's own chat-widget
        // render-state extraction
        // applies it, scaling both axes uniformly — no
        // further HUD-side factor. Called through the free function rather than
        // recomputed inline so this draw and [`HudRenderer::suggestion_layout`]'s
        // pointer hit-test (called from outside this function) cannot resolve
        // two different numbers from two copies of the same formula.
        let chat_open = frame.chat_input.is_some();
        let opts = frame.chat_options;
        let chat_pose_scale = chat_pose_scale(opts);
        let chat_line_h = chat_line_h(opts, chat_pose_scale);
        // `chat_width_px`/`chat_height_px` are vanilla's own
        // `ChatComponent.getWidth`/`getHeight` formulas, in the same
        // logical-canvas pixel unit as `b.w`/`b.h` (see their doc comments),
        // so no further conversion is needed to compare them against `b.w`.
        let chat_box_w = chat_width_px(opts.width_pct.clamp(0.0, 1.0)).min(b.w);
        // The chat column's own left inset — see [`CHAT_TEXT_INSET`]. Every
        // chat x below is derived from this one expression rather than from
        // `margin`, which is the HUD-wide constant the two chat surfaces were
        // wrongly sharing; the hit-test (`chat_interaction_at`) and the
        // suggestion popup's anchor (`suggestion_layout`) derive it the same
        // way, because a hit-test built from a second copy of a text origin is
        // a hit-test that drifts from what the player sees.
        let chat_inset = CHAT_TEXT_INSET * chat_pose_scale;
        // The plate is anchored at the screen edge and is wider than the text
        // column, so a full-width wrapped line cannot overhang its own
        // background — see [`CHAT_PLATE_PAD_PX`].
        let chat_plate_w = chat_box_w + CHAT_PLATE_PAD_PX * chat_pose_scale;
        let chat_height_pct = if chat_open {
            opts.height_pct_focused
        } else {
            opts.height_pct_unfocused
        };
        let chat_box_h = chat_height_px(chat_height_pct.clamp(0.0, 1.0));
        // `textOpacity = chatOpacity * 0.9 + 0.1` —
        // never fully transparent even at `chatOpacity == 0.0`.
        let chat_text_opacity = opts.text_opacity.clamp(0.0, 1.0).mul_add(0.9, 0.1);
        let chat_bg_opacity = opts.background_opacity.clamp(0.0, 1.0);
        // Through [`chat_input_top`], not a second `b.h - margin - …`, because
        // `suggestion_layout` places the dropdown relative to this row and is
        // called from outside this function too (the pointer hit-test).
        let input_y = chat_input_top(b.h, chat_pose_scale);
        if let Some(input) = frame.chat_input {
            // A translucent strip so text stays legible over bright terrain.
            // Vanilla's real `EditBox` has no equivalent knob of its own; this
            // reuses `chat_bg_opacity` rather than inventing an unread
            // constant, since it is the same "background behind chat text"
            // concept as the scrollback rows just below.
            // Derived from the *same* `input_y` and `chat_pose_scale` the text
            // draw below uses, so the strip and the glyphs cannot disagree.
            // Vanilla's band is `fill(2, height - 14, width - 2, height - 2, …)`
            // with the `EditBox`'s text at `height - 12`
            // (`:56`) — i.e. symmetric 2px padding around the text — so the
            // padding is 2 units, scaled with everything else.
            //
            // The previous version was `input_y - 3.0` tall by `chat_line_h`, and
            // was wrong twice: the `-3.0` was **unscaled** while the height was
            // scaled, so the band drifted off the text as chat scale rose; and it
            // began *above* `input_y`, which is where the scrollback's own
            // translucent rows end, so the two blacks overlapped and that seam
            // rendered at double opacity while the last rows of the glyph box had
            // no background at all.
            //
            // Width is the plate's, not the text column's: the typed line is
            // inset by `chat_inset` and may run the full column width, so a
            // strip only as wide as the column would end under the text. Same
            // defect as the scrollback rows below, same fix.
            b.rect_px(
                0.0,
                input_y - INPUT_STRIP_PAD * chat_pose_scale,
                chat_plate_w,
                glyph_h * chat_pose_scale + 2.0 * INPUT_STRIP_PAD * chat_pose_scale,
                [0.0, 0.0, 0.0, chat_bg_opacity],
            );
            // `EditBox.extractWidgetRenderState` paints a selection after the
            // background and before its glyphs. The input owns the same
            // character-index range; convert it with the exact text-width
            // function the following glyph draw uses. Clamp the endpoints to
            // both the input and its plate so an empty/stale range or a long
            // line cannot emit an invalid or off-strip fill.
            if let Some((from, to)) = frame.chat_selection {
                let char_len = input.chars().count();
                let from = from.min(char_len);
                let to = to.min(char_len);
                if from != to {
                    let left_text = input.chars().take(from).collect::<String>();
                    let right_text = input.chars().take(to).collect::<String>();
                    let left = (chat_inset + b.text_width(&left_text, chat_pose_scale))
                        .clamp(chat_inset, chat_plate_w);
                    let right = (chat_inset + b.text_width(&right_text, chat_pose_scale))
                        .clamp(chat_inset, chat_plate_w);
                    let (left, right) = if left <= right { (left, right) } else { (right, left) };
                    if right > left {
                        b.rect_px(
                            left,
                            input_y,
                            right - left,
                            glyph_h * chat_pose_scale,
                            CHAT_SELECTION,
                        );
                    }
                }
            }
            // No leading `>` — vanilla's own chat-screen input widget draws no
            // prompt glyph at all, just the typed text and a caret.
            // The typed line itself is always plain (input filters `§`), so a
            // flat, non-legacy draw is right, and at **full** opacity — vanilla
            // never multiplies the input `EditBox`'s own text by `chatOpacity`,
            // which only governs the scrollback below.
            //
            // Vanilla splits this into two draws — the text before the caret at
            // `textX`, then the text after it at `drawX` — but `drawX` is
            // `textX + width(before) + 1`, and the insert case immediately takes
            // that pixel back (`drawX--`), so the two halves land exactly where a
            // single draw of the whole string puts them. The split only *moves*
            // glyphs in the append case, where the second half is empty. One draw
            // is therefore faithful, not an approximation.
            b.text(input, chat_inset, input_y, chat_pose_scale, [1.0, 1.0, 1.0, 1.0]);
            // The highlighted suggestion, previewed in grey **behind** the
            // caret — `EditBox.extractRenderState`'s
            // `graphics.text(font, suggestion, cursorX - 1, textY, -8355712,
            // this.textShadow)`. Three things a previous fix here got wrong,
            // all because it measured the pen against `{input}_` or against
            // `font.width(value)` alone instead of vanilla's real `cursorX`:
            //
            // 1. **`cursorX` is `font.width(value)` — the typed text alone,
            //    not the text plus the caret glyph.** The caret contributes
            //    *no* advance in vanilla, because there it is a separately
            //    blinking overlay rectangle, never part of the measured
            //    string. Reserving the caret glyph's own width (`{input}_`)
            //    landed the ghost one whole underscore-width too far right,
            //    *permanently* — stable, and wrong: the owner's own report
            //    was "it's supposed to be behind [the caret], not pushing it
            //    to the right".
            // 2. **`cursorX` is `font.width(value) + 1`, not `font.width(value)`
            //    alone.** `EditBox.extractWidgetRenderState` computes
            //    `drawX += this.font.width(charSequence) + 1;` *before* setting
            //    `cursorX = drawX` — a full pixel reserved after the typed text,
            //    present in the **appended** (non-insert) case this shell always
            //    hits (see the `!insert` note below). This crate's own menu
            //    `EditBox` port already carries this exact `+ 1.0`
            //    (`crates/lodestone-shell/src/menu/edit_box.rs`'s
            //    `draw_state_with`); this draw site had not. Missing it made the
            //    ghost (`cursorX - 1`) land one pixel *short* of vanilla's real
            //    position — flush against, and in practice overlapping, the
            //    typed text's last glyph — and made the caret (`cursorX`) sit
            //    flush against the text instead of the one clear pixel vanilla
            //    leaves for it. The two errors netted to the same wrong ghost
            //    position from two different mistakes: reserving the caret's
            //    width (error 1, since fixed) versus never reserving vanilla's
            //    own one-pixel gap (error 2, fixed here).
            // 3. **The suggestion must be drawn *before* the caret, not
            //    after**, so the caret glyph composites on top and the two
            //    overlap by design (vanilla's own edit-box widget's render order is text →
            //    hint → suggestion → highlight → cursor). The previous fix
            //    drew `{input}{caret}` as one string, then the ghost after —
            //    on top of the caret, backwards from vanilla either way.
            //
            // `insert` is vanilla's `insert = cursorPos < value.length() ||
            // value.length() >= maxLength`. **Both** disjuncts are live here.
            // The first used to be treated as permanently false, on the (then
            // true, since stale) grounds that `ChatInput` only edited at the end
            // of the line; it grew Left/Right/Home/End caret motion, and this
            // draw never learned, which is why the indicator stayed pinned to
            // the end of the line while the insertion point moved. The second is
            // the `ChatInput`'s own 256-`char` cap
            // ([`crate::chat::MAX_CHAT_LENGTH`]), which also suppresses the
            // suggestion ghost once the line is full, matching vanilla rather
            // than overlapping the last few glyphs.
            //
            // `cursor_x` is vanilla's own cursor-x coordinate, which is the running
            // draw cursor *after* the
            // first half of the text has been drawn: it starts at the text
            // origin, then advances by the width of the text **before
            // the caret**, plus one — not the whole value, which is the second half of the
            // same bug. It then steps back one pixel when the caret is in insert mode.
            //
            // The `+ 1` gap is conditional in vanilla, not unconditional:
            // that trailing pixel is only added inside vanilla's own
            // non-empty-visible-text guard, in its own widget render-state
            // extraction,
            // and the guard is on the whole visible slice rather than on the
            // half — so an empty line reserves no pixel at all and the cursor
            // stays at the text origin, while a caret at position 0 of a
            // *non*-empty line reserves the pixel and then hands it straight
            // back through the insert-mode step-back. `text_width("")` is already `0.0`, but
            // adding `chat_pose_scale` unconditionally would still reserve a
            // pixel vanilla does not for the empty case, so the `is_empty` guard
            // matters even though the width term alone would not have.
            //
            // The chat input has no horizontal scroll of its own — vanilla's
            // `displayPos` is always 0 here, because the box is sized so the
            // whole 256-`char` budget fits (see [`crate::chat::ChatInput`]'s own
            // note on that) — so `before` is the real prefix rather than a
            // window into one, and no `relCursorPos` clamp is needed.
            let char_len = input.chars().count();
            let cursor_chars = frame.chat_cursor.unwrap_or(char_len).min(char_len);
            let before: String = input.chars().take(cursor_chars).collect();
            let full = char_len >= crate::chat::MAX_CHAT_LENGTH;
            let insert = cursor_chars < char_len || full;
            let cursor_x = if input.is_empty() {
                chat_inset
            } else {
                chat_inset + b.text_width(&before, chat_pose_scale) + chat_pose_scale
            } - if insert { chat_pose_scale } else { 0.0 };
            if !full && let Some(ghost) = frame.chat_suggestion_ghost {
                b.text(
                    ghost,
                    cursor_x - chat_pose_scale,
                    input_y,
                    chat_pose_scale,
                    SUGGESTION_GHOST,
                );
            }
            // The caret, drawn **last** so it composites on top of both the
            // typed text and any ghost suggestion — the literal fix for the
            // draw-order bug above. `chat_caret_visible` blinks it at vanilla's
            // real 300ms rate (see [`HudFrame::chat_caret_visible`]).
            //
            // Two shapes, chosen by `insert`, exactly as
            // `EditBox.extractWidgetRenderState`'s final block chooses between
            // `TextCursorUtils.extractInsertCursor` and `extractAppendCursor`:
            //
            // - **append** (`_`): the literal underscore *character*, drawn as
            //   text at `cursorX`, so it sits under the trailing edge of the
            //   line. One pixel right of the ghost's
            //   `cursor_x - chat_pose_scale`, which is vanilla's own one-pixel
            //   gap between the two, not a restated offset.
            // - **insert** (`|`): a 1 px-wide filled bar spanning the line, from
            //   one pixel above the glyph box to one pixel below it —
            //   `fill(x, y - 1, x + 1, y + lineHeight, color)` with
            //   `lineHeight == 9 + 1` against 9-tall vanilla glyphs, i.e. the
            //   glyph height plus two. Reproduced here against this font's own
            //   `glyph_h` so the bar keeps matching the text it sits between
            //   rather than inheriting vanilla's glyph metric.
            //
            // Both scale with `chat_pose_scale`, which is this surface's
            // stand-in for one vanilla pixel.
            if frame.chat_caret_visible {
                if insert {
                    b.rect_px(
                        cursor_x,
                        input_y - chat_pose_scale,
                        chat_pose_scale,
                        (glyph_h + 2.0) * chat_pose_scale,
                        [1.0, 1.0, 1.0, 1.0],
                    );
                } else {
                    b.text("_", cursor_x, input_y, chat_pose_scale, [1.0, 1.0, 1.0, 1.0]);
                }
            }
        }
        // The scrollback stacks upward from here — vanilla's own chat-bottom coordinate
        // (see [`chat_bottom`]'s doc), unconditional on whether the box is
        // open. This used to be `input_y - INPUT_STRIP_PAD * chat_pose_scale`
        // while open, which coupled the scrollback's anchor to the input
        // strip's own top edge; vanilla never does that (`chatBottom` and the
        // `EditBox`'s `height - 12` are two independent literals in two
        // different classes), and the coupling is what erased vanilla's real
        // ~26px headroom between the newest message and the input box,
        // leaving them flush.
        let chat_bottom = chat_bottom(b.h, chat_pose_scale);
        // How many visual rows fit the configured box height — vanilla's
        // `ChatComponent.getLinesPerPage` (vanilla's own chat-component rendering,
        // `height / lineHeight`), derived from the same `chat_box_h`/
        // `chat_line_h` the draw below actually uses, not a restated
        // constant.
        let max_visual_rows = (chat_box_h / chat_line_h).floor().max(1.0) as usize;
        let mut row_i = 0usize;
        // [`HudFrame::chat_spans`] is the alternative, hex-carrying source; see
        // its own doc for why a non-empty one wins outright rather than being
        // composited with `chat`. Everything below this `if` mirrors the `else`
        // branch move for move — same fade, same cache shape, same stacking —
        // with a `Vec<TextSpan>` in place of a `§`-coded `String` at each step,
        // because the *decision* logic (fade, row budget, stacking order) does
        // not care which draw vocabulary carries the text.
        if !frame.chat_spans.is_empty() {
            'entries_spans: for (spans, age) in frame.chat_spans.iter().rev() {
                let alpha = if chat_open {
                    1.0
                } else {
                    chat_line_alpha(*age)
                };
                if alpha <= 0.0 {
                    break;
                }
                // The span sibling of `strip_legacy`: vanilla's
                // `ChatFormatting.stripFormatting` removes every `§`+code pair
                // wholesale (colour and the five format flags alike), which
                // for a span list already past that decode is "every run
                // draws with `TextStyle::default()`".
                let stripped: Option<Vec<TextSpan>> =
                    (!opts.colors).then(|| strip_style_spans(spans));
                let display: &[TextSpan] = stripped.as_deref().unwrap_or(spans);
                let sub_rows = match frame.chat_wrap_spans {
                    Some(cache) => cache.rows(display, chat_box_w, chat_pose_scale, |t| {
                        b.wrap_spans(t, chat_box_w, chat_pose_scale)
                    }),
                    None => std::rc::Rc::from(b.wrap_spans(display, chat_box_w, chat_pose_scale)),
                };
                for sub in sub_rows.iter().rev() {
                    if row_i >= max_visual_rows {
                        break 'entries_spans;
                    }
                    let y = chat_bottom - (row_i as f32 + 1.0) * chat_line_h;
                    if y < margin {
                        break 'entries_spans;
                    }
                    b.rect_px(
                        0.0,
                        y - 1.0,
                        chat_plate_w,
                        chat_line_h,
                        [0.0, 0.0, 0.0, chat_bg_opacity * alpha],
                    );
                    b.text_spans(
                        sub,
                        chat_inset,
                        y,
                        chat_pose_scale,
                        [0.92, 0.94, 1.0],
                        alpha * chat_text_opacity,
                    );
                    row_i += 1;
                }
            }
        } else {
            // Each logical entry can wrap into several visual rows, all sharing
            // that entry's age/alpha. Vanilla stacks a wrapped message's *last*
            // split line nearest the bottom edge and its earlier lines above it
            // (`ChatComponent.addMessageToDisplayQueue`'s per-line `addFirst`,
            // vanilla's own chat-component rendering, combined with `forEachLine`'s
            // `lineIndex → chatBottom - lineIndex * entryHeight`,
            // vanilla's own chat-component rendering) — reversing each entry's own wrapped
            // rows before stacking reproduces that order.
            'entries: for (line, age) in frame.chat.iter().rev() {
                // While open, every line is fully lit; while closed, lines fade over
                // their last two seconds of a ten-second life and then disappear.
                let alpha = if chat_open {
                    1.0
                } else {
                    chat_line_alpha(*age)
                };
                if alpha <= 0.0 {
                    // Older-than-visible lines end the stack: everything above is
                    // older still, so there is nothing more to draw.
                    break;
                }
                // `options.chat.color == false` strips every legacy code before
                // wrapping/drawing (`ComponentRenderUtils.stripColor`) rather than
                // just ignoring them while drawing, matching vanilla.
                let stripped = if opts.colors { None } else { Some(strip_legacy(line)) };
                let display: &str = stripped.as_deref().unwrap_or(line);
                // Wrapped once per message, not once per frame:
                // the cache keys on the display text plus this frame's box width
                // and pose scale, so a frame with no new line, no resize and no
                // options edit performs zero wraps. Without a cache, this work
                // repeats on every frame.
                let sub_rows = match frame.chat_wrap {
                    Some(cache) => cache.rows(display, chat_box_w, chat_pose_scale, |t| {
                        b.wrap_legacy(t, chat_box_w, chat_pose_scale)
                    }),
                    None => std::rc::Rc::from(b.wrap_legacy(display, chat_box_w, chat_pose_scale)),
                };
                for sub in sub_rows.iter().rev() {
                    if row_i >= max_visual_rows {
                        break 'entries;
                    }
                    let y = chat_bottom - (row_i as f32 + 1.0) * chat_line_h;
                    if y < margin {
                        break 'entries;
                    }
                    b.rect_px(
                        0.0,
                        y - 1.0,
                        chat_plate_w,
                        chat_line_h,
                        [0.0, 0.0, 0.0, chat_bg_opacity * alpha],
                    );
                    b.text_legacy(
                        sub,
                        chat_inset,
                        y,
                        chat_pose_scale,
                        [0.92, 0.94, 1.0],
                        alpha * chat_text_opacity,
                    );
                    row_i += 1;
                }
            }
        }

        // The scrollback's own scroll indicator — vanilla's
        // `ChatComponent.extractRenderState`'s `if (total > 0 && isForeground)`
        // block. Drawn only while open
        // (`isForeground`) and only once there is more history than fits on
        // screen (`virtualHeight != chatHeight`), matching vanilla's own two
        // gates. The two colours are vanilla's literal `13382451`/`3355562`
        // (`0xCC3333`/`0x3333AA`); the alpha vanilla derives from a sign check
        // on an internal `y` that is only ever positive for a canvas smaller
        // than the chat box itself is simplified here to a single fixed
        // `170/255` (vanilla's own "not that" branch) rather than guessed at
        // — a named simplification, not a hidden one.
        if let Some(bar) = frame.chat_scrollbar {
            let rows_per_page = chat_lines_per_page(opts, chat_pose_scale, chat_open);
            let total = bar.total as f32;
            let count = (bar.total.min(rows_per_page)) as f32;
            if chat_open && total > 0.0 && count < total {
                let chat_h = count * chat_line_h;
                let virtual_h = total * chat_line_h;
                let thumb_h = (chat_h * chat_h / virtual_h).max(1.0);
                let thumb_bottom = chat_bottom - bar.scrolled as f32 * chat_h / total;
                let thumb_top = thumb_bottom - thumb_h;
                // Clear of the widest wrapped line rather than 2 px past the
                // column's right edge — the text now starts at `chat_inset`, so
                // a full-width row ends at `chat_inset + chat_box_w` and the
                // old offset put the bar on top of its last glyphs. See
                // [`CHAT_SCROLLBAR_GAP`].
                let x = chat_box_w + CHAT_SCROLLBAR_GAP * chat_pose_scale;
                let rgb = if bar.new_message_since_scroll {
                    [0xCC as f32 / 255.0, 0x33 as f32 / 255.0, 0x33 as f32 / 255.0]
                } else {
                    [0x33 as f32 / 255.0, 0x33 as f32 / 255.0, 0xAA as f32 / 255.0]
                };
                b.rect_px(
                    x,
                    thumb_top,
                    2.0 * chat_pose_scale,
                    thumb_h,
                    [rgb[0], rgb[1], rgb[2], 170.0 / 255.0],
                );
                // Vanilla's second, 1px `0xCCCCCC` highlight fill just right
                // of the main bar.
                b.rect_px(
                    x + 2.0 * chat_pose_scale,
                    thumb_top,
                    1.0 * chat_pose_scale,
                    thumb_h,
                    [0xCC as f32 / 255.0, 0xCC as f32 / 255.0, 0xCC as f32 / 255.0, 170.0 / 255.0],
                );
            }
        }

        // The command-suggestion dropdown, last in the chat overlay because it
        // overlaps both the input line above and the scrollback below —
        // `ChatScreen.extractRenderState` calls `commandSuggestions
        // .extractRenderState` after `super`, i.e. after every widget including
        // the `EditBox`. `draw_command_suggestions`' own doc holds the table of
        // what must still composite above it, and `SUGGESTION_LAYERS` the order
        // inside it.
        if let Some(popup) = frame.chat_suggestions.as_ref() {
            let layout =
                suggestion_layout(b.w, b.h, chat_pose_scale, popup, |s| {
                    b.text_width(s, chat_pose_scale)
                });
            draw_command_suggestions(&mut b, popup, layout, chat_pose_scale, &SUGGESTION_LAYERS);
        }

        // Crosshair: a white plus at the centre, drawn through the inverting
        // blend (`HudRenderer::invert_pipeline`), so it shows as the inverse of
        // whatever is behind it, as vanilla's does.
        //
        // `arm`/`thick` reproduce vanilla's actual ink, not its sprite's bounding
        // box. `Hud.extractCrosshair` blits the 15x15
        // `hud/crosshair` sprite (`assets/minecraft/textures/gui/sprites/hud/
        // crosshair.png`) at `((guiWidth-15)/2, (guiHeight-15)/2, 15, 15)` — but
        // that box is mostly transparent padding. Read directly off the PNG's own
        // pixels: only rows/columns 3..=11 of the 15x15 grid are opaque, a
        // single-pixel-thick "+" spanning 9 of the 15 px, centred in the box (and
        // therefore already centred on `(cx, cy)` here). This used to draw the
        // *sprite's* 16-wide, 2-thick bounding box solid instead of that 9-wide,
        // 1-thick mark — a ~3.5x ink-area crosshair at every GUI scale, on both
        // targets (this call has no `wasm32` branch), which is the whole of the
        // "crosshair is too big" report. Hand-drawn rather than a real `b.sprite`
        // blit for the same reason the attack-indicator fallback below is: this
        // stays a plain colour-stream quad pair, so it still draws with no GUI
        // atlas attached (a jar-less/headless run), which `b.sprite` would not,
        // and the two tests asserting an exact 12-vert/2-quad crosshair
        // (`geometry_has_crosshair_and_text`, `hiding_the_debug_overlay_removes_its_geometry`)
        // stay meaningful rather than moving to a different vertex stream.
        if frame.crosshair {
            let (cx, cy) = (b.w * 0.5, b.h * 0.5);
            let arm = 4.5;
            let thick = 1.0;
            let col = [1.0, 1.0, 1.0, 1.0];
            let start = (b.verts.len() / FLOATS_PER_VERTEX) as u32;
            b.rect_px(cx - arm, cy - thick * 0.5, arm * 2.0, thick, col);
            b.rect_px(cx - thick * 0.5, cy - arm, thick, arm * 2.0, col);
            b.crosshair = Some(start..(b.verts.len() / FLOATS_PER_VERTEX) as u32);

            // Attack-strength (cooldown) indicator: the crosshair variant is a
            // small left-to-right fill bar just below the crosshair. `Off`
            // suppresses both indicators; `Hotbar` is rendered separately
            // beside the hotbar. The menu cycles these three `AttackIndicator`
            // options, and this branch handles only `Crosshair`. Native 16x4,
            // anchored at `(cx - 8, cy + 9)` against this canvas's centre,
            // which this block already computed for the plus above.
            //
            // `b.sprite`/`b.gui_geometry` are no-op-safe with no atlas attached
            // (see `sprite_vitals`'s doc on the same pattern), so a
            // jar-less/headless run draws nothing here rather than needing a
            // second procedural implementation — the same choice already made
            // for the underwater bubble row (`bubble_row`, below).
            //
            // **Gated on `AttackIndicator::Crosshair`**, which is vanilla's own
            // `if (options.attackIndicator().get() == CROSSHAIR)` around this
            // whole block. `Off` draws nothing; `Hotbar` draws a different
            // gauge beside the hotbar instead (see the `hotbar_attack_indicator`
            // block further down) — never both, which is why each site tests for
            // its own variant rather than for "not off".
            //
            // Vanilla hides this entirely once `attackStrengthScale >= 1.0`
            // *unless* a slow weapon (delay > 5 ticks) is aimed at a living,
            // in-range target, in which case a distinct "ready" icon
            // (`CROSSHAIR_ATTACK_INDICATOR_FULL_SPRITE`) replaces it
            //. That icon needs the crosshair's target
            // entity plus its liveness/range/weapon-delay, none of which
            // `HudFrame` carries — deliberately out of scope per
            // `docs/combat.md`'s crits/sweep cut for the same issue. At full
            // charge this draws nothing, matching vanilla's non-"ready" case.
            if let (Some(raw_scale), crate::config::AttackIndicator::Crosshair) =
                (frame.attack_cooldown, frame.attack_indicator)
            {
                let scale = raw_scale.clamp(0.0, 1.0);
                if scale < 1.0 {
                    let iw = 16.0;
                    let ih = 4.0;
                    let ix = cx - iw * 0.5;
                    let iy = cy + 9.0;
                    let white = [1.0, 1.0, 1.0, 1.0];
                    b.sprite(
                        "hud/crosshair_attack_indicator_background",
                        ix,
                        iy,
                        iw,
                        ih,
                        white,
                    );
                    if scale > 0.0 {
                        // Crop by shrinking both the destination width and the
                        // sampled UV span, exactly `sprite_vitals`' XP-bar-progress
                        // idiom — reveals the fill pattern instead of squashing it.
                        for mut q in
                            b.gui_geometry("hud/crosshair_attack_indicator_progress", ix, iy, iw, ih)
                        {
                            let span = q.uv_max[0] - q.uv_min[0];
                            q.dst[2] *= scale;
                            q.uv_max[0] = q.uv_min[0] + span * scale;
                            b.push_sprite_quad(q, white);
                        }
                    }
                }
            }
        }

        // The hotbar (bottom-centre) and the survival pip rows above it. The
        // hotbar draws whenever we're in active play; the pips only on a live
        // survival server that reports health/food.
        let cx = b.w * 0.5;
        // With the vanilla GUI atlas attached, the vitals cluster (hotbar, XP,
        // hearts, hunger) draws from real sprites; without it — jar-less runs and
        // the headless negative-control path — it falls back to the procedural
        // quads below. Both branches draw the same vitals cluster, so the rest
        // of the HUD is oblivious to which path ran.
        if b.gui.is_some() {
            sprite_vitals(&mut b, frame, &anim);
        } else {
            let pip = 8.0;
            let gap = 2.0;
            let row_w = 10.0 * (pip + gap);

            // Hotbar: a 9-cell bar with the selected cell ringed in white. Item
            // icons are deferred (no item atlas yet) so the cells are empty wells —
            // the frame and selection are real, the contents explicitly aren't.
            let hotbar_top = if let Some(sel) = frame.hotbar {
                let sel = sel.min(8);
                let cell = 22.0;
                let hw = 9.0 * cell;
                let hx = cx - hw * 0.5;
                let hy = b.h - HOTBAR_MARGIN - cell;
                b.rect_px(
                    hx - 2.0,
                    hy - 2.0,
                    hw + 4.0,
                    cell + 4.0,
                    [0.0, 0.0, 0.0, 0.55],
                );
                for i in 0..9 {
                    let sx = hx + i as f32 * cell;
                    b.rect_px(
                        sx + 1.0,
                        hy + 1.0,
                        cell - 2.0,
                        cell - 2.0,
                        [0.28, 0.28, 0.30, 0.5],
                    );
                }
                // A 2px white ring around the selected cell (four edges).
                let sx = hx + sel as f32 * cell;
                let bw = 2.0;
                let col = [0.95, 0.97, 1.0, 0.95];
                b.rect_px(sx - 1.0, hy - 1.0, cell + 2.0, bw, col);
                b.rect_px(sx - 1.0, hy + cell + 1.0 - bw, cell + 2.0, bw, col);
                b.rect_px(sx - 1.0, hy - 1.0, bw, cell + 2.0, col);
                b.rect_px(sx + cell + 1.0 - bw, hy - 1.0, bw, cell + 2.0, col);
                hy
            } else {
                b.h - HOTBAR_MARGIN
            };

            // XP bar: a full-hotbar-width green progress bar just above the hotbar,
            // with the level number centred above it (vanilla green). Drawn only
            // once the server has sent experience (`frame.xp`); off a live server
            // this is `None` and nothing draws, keeping the gauge honest.
            // The same two gates the sprite path applies, for the same reasons — see
            // [`HudFrame::can_hurt_player`].
            //
            // This used to yield a `vitals_base` the pip rows stacked off, so an XP
            // bar and its level number pushed the hearts up. Both branches now
            // agree with [`sprite_vitals`] and take [`vitals_line_base`] instead —
            // vanilla's own line-base y-coordinate does not move for the XP bar, and having the
            // two paths disagree about that was the reason the air-row gate could
            // not derive one rect for both.
            if let Some((level, progress)) = frame.xp.filter(|_| frame.can_hurt_player) {
                let bar_w = 9.0 * 22.0;
                let bx = cx - bar_w * 0.5;
                let bar_h = 4.0;
                let by = hotbar_top - bar_h - 5.0;
                b.rect_px(bx, by, bar_w, bar_h, [0.0, 0.0, 0.0, 0.7]);
                let fill = bar_w * progress.clamp(0.0, 1.0);
                if fill > 0.0 {
                    b.rect_px(bx, by, fill, bar_h, [0.47, 0.82, 0.16, 1.0]);
                }
                if level > 0 {
                    // Vanilla metrics, not this function's ambient `scale`/
                    // `line_h` — the same fix [`sprite_vitals`]'s own copy of
                    // this number already documents: `scale` here made it
                    // twice vanilla's size, and `line_h` is this HUD's 5×7
                    // debug-font stride, not `ContextualBar`'s real `6px` gap
                    // above the bar's top (`by - 6.0`).
                    let s = level.to_string();
                    let tw = b.text_width(&s, 1.0);
                    b.text(
                        &s,
                        cx - tw * 0.5,
                        by - 6.0,
                        1.0,
                        [0.44, 0.92, 0.20, 1.0],
                    );
                }
            }

            // Health / food pip rows, on vanilla's own line-base y-coordinate — see
            // [`vitals_line_base`]. Each row is 10 pips of 2 units; a pip lights the
            // moment any of its two units is present (a deliberate simplification —
            // no half-pip art yet).
            let bars_y = vitals_line_base(b.h);
            let health_rows = heart_rows(frame.max_health);
            // The armour row, one row above the hearts and on the same left anchor,
            // mirroring [`sprite_vitals`]'s placement so the jar-less fallback and
            // the real thing agree about which side and which line it is on. `pips`
            // has no half-pip art (see the note below), so this row shows armour
            // rounded up to the pip, exactly as the health row already does — the
            // half-icon distinction only reaches pixels on the sprite path, and
            // [`armour_icon`] is the one place it is decided.
            //
            // `bars_y` is deliberately unchanged: it is the anchor the action bar and
            // the rest of the HUD hang off, and vanilla's own action bar sits at a
            // constant `guiHeight - 68` regardless of how many vitals rows are up.
            if frame.can_hurt_player
                && let Some(armour) = frame.armour
                && armour > 0
            {
                b.pips(
                    armour as f32,
                    cx - row_w - 8.0,
                    bars_y - health_rows as f32 * VITALS_ROW_PITCH,
                    pip,
                    gap,
                    [0.72, 0.76, 0.82, 1.0],
                );
            }
            if frame.can_hurt_player && let Some(hp) = frame.health {
                for row in 0..health_rows {
                    b.pips(
                        (hp - row as f32 * 20.0).max(0.0),
                        cx - row_w - 8.0,
                        bars_y - row as f32 * VITALS_ROW_PITCH,
                        pip,
                        gap,
                        [0.86, 0.15, 0.16, 1.0],
                    );
                }
            }
            if frame.can_hurt_player && let Some(food) = frame.food {
                b.pips(
                    food as f32,
                    cx + 8.0,
                    bars_y,
                    pip,
                    gap,
                    [0.78, 0.60, 0.20, 1.0],
                );
            }

        }

        // Item icons sit inside the hotbar cells, drawn over whichever hotbar
        // frame (real atlas or procedural) was emitted above.
        draw_hotbar_items(&mut b, frame, &anim);
        draw_hotbar_cooldowns(&mut b, frame);
        if let Some(selector) = frame.spectator_hotbar {
            spectator::draw(&mut b, selector);
        }

        // The **hotbar-anchored** attack-strength gauge — vanilla's
        // `AttackIndicatorStatus::HOTBAR` branch, which sits in `Hud`'s hotbar
        // section rather than beside the crosshair one, and is a genuinely
        // different draw from the crosshair variant: an 18x18 sprite pair
        // filling **bottom-up**, against the crosshair's 16x4 pair filling
        // left-to-right. The two are mutually exclusive by construction — each
        // site tests for its own variant, never for "not off" — which is
        // vanilla's own shape and the reason `Off` needs no third branch.
        //
        // Gated on `frame.hotbar` because vanilla draws it inside the block that
        // draws the hotbar itself, so a spectator or a hidden HUD gets neither.
        //
        // Anchored at vanilla's own `(guiWidth / 2 + 91 + 6, guiHeight - 20)`,
        // absolute against this canvas exactly as the action bar's
        // `guiHeight - 72` below already is. Vanilla mirrors that x to
        // `guiWidth / 2 - 91 - 22` when the **offhand** arm is the right one,
        // i.e. for a left-handed player; `mainHand` is an inactive row on this
        // client's settings tree and nothing else models a main arm for the local
        // player, so this takes the right-handed branch — vanilla's default —
        // rather than inventing a source for the fork.
        //
        // `b.sprite`/`b.gui_geometry` are no-op-safe with no atlas attached, the
        // same property the crosshair variant above relies on, so a jar-less or
        // headless run draws nothing here rather than needing a procedural
        // fallback.
        if let (Some(_), Some(raw_scale), crate::config::AttackIndicator::Hotbar) = (
            frame.hotbar,
            frame.attack_cooldown,
            frame.attack_indicator,
        ) {
            let scale = raw_scale.clamp(0.0, 1.0);
            if scale < 1.0 {
                let size = 18.0;
                let ix = b.w * 0.5 + 91.0 + 6.0;
                let iy = b.h - 20.0;
                let white = [1.0, 1.0, 1.0, 1.0];
                b.sprite(
                    "hud/hotbar_attack_indicator_background",
                    ix,
                    iy,
                    size,
                    size,
                    white,
                );
                // `(int)(attackStrengthScale * 19.0F)` — **19**, not 18, and
                // vanilla's own literal. With `scale < 1.0` already established
                // above it cannot exceed 18, so no clamp is needed; transcribing
                // it as 18 would leave the gauge one pixel short of full at every
                // value.
                let progress = (scale * 19.0).floor();
                if progress > 0.0 {
                    // Vanilla's blit is
                    // `(texW 18, texH 18, u 0, v 18 - progress, x, y + 18 -
                    // progress, w 18, h progress)` — it samples the **bottom**
                    // `progress` rows and lands them at the bottom of the box, so
                    // the gauge fills upward. Cropping the v span and the
                    // destination y together is `sprite_vitals`' XP-bar idiom
                    // rotated a quarter turn; shrinking only the height would
                    // squash the whole sprite instead of revealing part of it.
                    for mut q in
                        b.gui_geometry("hud/hotbar_attack_indicator_progress", ix, iy, size, size)
                    {
                        let span = q.uv_max[1] - q.uv_min[1];
                        let fraction = progress / size;
                        q.uv_min[1] = q.uv_max[1] - span * fraction;
                        q.dst[1] += size - progress;
                        q.dst[3] = progress;
                        b.push_sprite_quad(q, white);
                    }
                }
            }
        }

        // Action bar: a single centred line above the vitals cluster, fading with
        // the server-driven alpha. Legacy `§` colour codes render.
        //
        // Unscaled: `extractOverlayMessage` makes **no** `pose().scale()` call at
        // all, like the held-item name below. This used `scale`, which is 2.0 —
        // and since `logical_canvas` has already divided by the GUI scale, that
        // was a flat 2x on top of vanilla's own factor. See
        // `docs/hud-text-scale.md`.
        //
        // GUI height minus 72, absolute, the way the held-item name below already
        // reads its own GUI-height-minus-59: vanilla's own overlay-message
        // extraction translates the
        // pose to horizontal centre, GUI-height-minus-68, and then draws four
        // pixels further up, and
        // it takes no game-mode or vitals-row branch of any kind. This used to
        // hang off `bars_y`, which meant it moved with the vitals cluster — so
        // correcting the baseline above would otherwise have dragged it a further
        // 3 px away from vanilla rather than leaving it alone.
        //
        // Not ported: `textWithBackdrop`'s translucent panel behind the glyphs.
        if let Some((msg, alpha)) = frame.action_bar.as_ref().filter(|(_, a)| *a > 0.0) {
            // `spans_width`/`text_spans`, not the `legacy_width`/`text_legacy`
            // pair: a `§` string cannot express a hex colour, so the producer now
            // hands over spans and the measure has to be the one that matches the
            // draw or a centred line lands at the wrong `x`.
            let tw = b.spans_width(msg, 1.0);
            b.text_spans(
                msg,
                cx - tw * 0.5,
                b.h - 72.0,
                1.0,
                [1.0, 1.0, 1.0],
                *alpha,
            );
        }

        // Held-item name: the selected hotbar item's styled name,
        // above the hotbar, fading with a server-independent client timer.
        // Unlike the action bar and title, vanilla draws this **unscaled**
        // (vanilla's own hud rendering, a plain `graphics.textWithBackdrop` call, no
        // ×2) — the same "vanilla's own draw never scales the font" lesson
        // the XP level number's fix already established two
        // blocks up in [`sprite_vitals`]. Using `scale` here would repeat
        // that exact defect on a second piece of HUD text.
        // `held_item_spans` is the hex-carrying sibling (see its own doc) and
        // wins when non-empty, matching `chat_spans`'s convention; a caller
        // that has not been threaded through to the spans source yet still
        // draws via the legacy `held_item` path below.
        if let Some((spans, alpha)) = frame.held_item_spans.as_ref().filter(|(_, a)| *a > 0.0) {
            let tw = b.spans_width(spans, 1.0);
            let x = (b.w - tw) * 0.5;
            let y = b.h - 59.0 + if frame.can_hurt_player { 0.0 } else { 14.0 };
            b.text_spans(spans, x, y, 1.0, [1.0, 1.0, 1.0], *alpha);
        } else if let Some((name, alpha)) = frame.held_item.as_ref().filter(|(_, a)| *a > 0.0) {
            let tw = b.legacy_width(name, 1.0);
            let x = (b.w - tw) * 0.5;
            // `extractSelectedItemName`: `y = guiHeight - 59`, then `y += 14` when
            // `!canHurtPlayer()`, because creative and spectator have no
            // health/hunger row for the label to clear.
            let y = b.h - 59.0 + if frame.can_hurt_player { 0.0 } else { 14.0 };
            b.text_legacy(name, x, y, 1.0, [1.0, 1.0, 1.0], *alpha);
        }

        // Title / subtitle: a large centred overlay mid-screen, fading with the
        // server-driven alpha. Drawn only while a server-sent title is active,
        // so it costs nothing off a server that sends none.
        // `extractTitle` translates once to the screen centre
        // (`:376`), then draws each string at an offset *inside* its own pose
        // scale — title `scale(4.0)` at `y = -10` (`:378,381`), subtitle
        // `scale(2.0)` at `y = 5` (`:385,387`). Multiplied out, those are the two
        // anchors below.
        //
        // Vanilla's factors are used **whole**. Multiplying them by this HUD's
        // `scale` drew both at 2x (`logical_canvas` has already applied the GUI
        // scale, so `scale` is a second application) and, worse, made the
        // subtitle's offset depend on the *title's* scale via `ty + ts * 9.0` —
        // so correcting the scale alone would have moved the subtitle. The
        // position was independently wrong too: `b.h * 0.40` is not `h/2 - 40`.
        if let Some((title, subtitle, alpha)) = frame.title.as_ref().filter(|(_, _, a)| *a > 0.0) {
            const TITLE_POSE: f32 = 4.0;
            const SUBTITLE_POSE: f32 = 2.0;
            let cy = b.h * 0.5;
            // Spans, so a hex-coloured title keeps its colour — see
            // [`HudFrame::title`]. `spans_width` is the measurement half of
            // `text_spans`, and using the other pair here would shift every
            // centred glyph.
            let tw = b.spans_width(title, TITLE_POSE);
            b.text_spans(
                title,
                (b.w - tw) * 0.5,
                cy - 10.0 * TITLE_POSE,
                TITLE_POSE,
                [1.0, 1.0, 1.0],
                *alpha,
            );
            if let Some(sub) = subtitle {
                let sw = b.spans_width(sub, SUBTITLE_POSE);
                b.text_spans(
                    sub,
                    (b.w - sw) * 0.5,
                    cy + 5.0 * SUBTITLE_POSE,
                    SUBTITLE_POSE,
                    [1.0, 1.0, 1.0],
                    *alpha,
                );
            }
        }

        // Boss bars: stacked title-over-bar at the top-centre —
        // `BossHealthOverlay.extractRenderState`/`extractBar`, ported at
        // vanilla's own fixed 182×5 native size and `BOSS_BAR_TEXT_SCALE`
        // (`1.0`) rather than this function's ambient `scale`/`line_h`, the
        // same exemption as [`SIDEBAR_LINE_H`]. An empty slice draws nothing,
        // so this costs zero verts off a server that sends none.
        //
        // Four clauses, each a real vanilla `blitSprite`, all untinted
        // (`color = -1` in `extractBar`'s private overload) since every
        // colour is its own pre-baked sprite rather than a tinted greyscale
        // one:
        //   1. the background plate, full 182px, `bb.color.background_sprite_id()`
        //   2. the background notch overlay, also full 182px, only when the
        //      bar's overlay style is not `Progress`
        //   3. the progress fill, `bb.color.progress_sprite_id()`, **cropped**
        //      (not scaled) to `lerp_discrete_width(progress, 182)` px — see
        //      that function's doc for why this differs from a plain
        //      `progress * 182`
        //   4. the progress notch overlay, cropped to the same width as (3),
        //      again only when the overlay style is not `Progress`
        // (2) and (4) draw on top of (1) and (3) respectively, exactly
        // `extractBar`'s draw order.
        if !frame.boss_bars.is_empty() {
            let bscale = BOSS_BAR_TEXT_SCALE;
            let bar_x = b.w * 0.5 - BOSS_BAR_WIDTH * 0.5;
            let mut y_offset = BOSS_BAR_TOP;
            let white = [1.0, 1.0, 1.0, 1.0];
            for bb in frame.boss_bars {
                let yo = y_offset;
                let tw = b.spans_width(&bb.title, bscale);
                b.text_spans(
                    &bb.title,
                    b.w * 0.5 - tw * 0.5,
                    yo - 9.0,
                    bscale,
                    [1.0, 1.0, 1.0],
                    1.0,
                );

                // (1) background plate, full width.
                b.sprite(
                    bb.color.background_sprite_id(),
                    bar_x,
                    yo,
                    BOSS_BAR_WIDTH,
                    BOSS_BAR_HEIGHT,
                    white,
                );
                // (2) background notch overlay, full width, on top of (1).
                if let Some(id) = bb.overlay.background_sprite_id() {
                    b.sprite(id, bar_x, yo, BOSS_BAR_WIDTH, BOSS_BAR_HEIGHT, white);
                }

                let width_px = crate::overlay::lerp_discrete_width(
                    bb.progress.clamp(0.0, 1.0),
                    BOSS_BAR_WIDTH as i32,
                );
                if width_px > 0 {
                    let frac = width_px as f32 / BOSS_BAR_WIDTH;
                    // (3) progress fill, cropped to `frac` — shrink both the
                    // destination width and the sampled UV span (as the XP
                    // bar's fill does above), so the bar reveals its own
                    // pattern instead of squashing it into a narrower box.
                    for mut q in
                        b.gui_geometry(bb.color.progress_sprite_id(), bar_x, yo, BOSS_BAR_WIDTH, BOSS_BAR_HEIGHT)
                    {
                        let span = q.uv_max[0] - q.uv_min[0];
                        q.dst[2] *= frac;
                        q.uv_max[0] = q.uv_min[0] + span * frac;
                        b.push_sprite_quad(q, white);
                    }
                    // (4) progress notch overlay, cropped the same way, on
                    // top of (3).
                    if let Some(id) = bb.overlay.progress_sprite_id() {
                        for mut q in b.gui_geometry(id, bar_x, yo, BOSS_BAR_WIDTH, BOSS_BAR_HEIGHT) {
                            let span = q.uv_max[0] - q.uv_min[0];
                            q.dst[2] *= frac;
                            q.uv_max[0] = q.uv_min[0] + span * frac;
                            b.push_sprite_quad(q, white);
                        }
                    }
                }
                y_offset += BOSS_BAR_STEP;
                if y_offset >= b.h / 3.0 {
                    break;
                }
            }
        }

        // Scoreboard sidebar — `Hud.displayScoreboardSidebar`, ported at vanilla's
        // own metrics (`SIDEBAR_LINE_H`/`SIDEBAR_TEXT_SCALE`) rather than this
        // function's ambient `scale`/`line_h`, exactly the exemption
        // [`TAB_LINE_H`] documents for the tab list. `width` is the widest of the
        // title and every `name [+ ": " + score]` row (the spacer only counts
        // when the row actually has a score — vanilla's
        // `scoreWidth > 0 ? spacerWidth + scoreWidth : 0`); `bottom` sits at
        // `guiHeight() / 2 + height / 3`, which is a deliberate top bias, not a
        // symmetric centring — porting it as `h/2` would silently "fix" a
        // vanilla quirk. Absent when nothing is displayed.
        if let Some(side) = frame.sidebar {
            let sscale = SIDEBAR_TEXT_SCALE;
            let spacer_w = b.text_width(": ", sscale);
            let title_w = b.spans_width(&side.title, sscale);
            let mut width = title_w;
            for l in &side.lines {
                let score_w = b.spans_width(&l.score, sscale);
                let extra = if score_w > 0.0 { spacer_w + score_w } else { 0.0 };
                width = width.max(b.spans_width(&l.label, sscale) + extra);
            }
            let entries = side.lines.len() as f32;
            let height = entries * SIDEBAR_LINE_H;
            let bottom = b.h / 2.0 + height / 3.0;
            let left = b.w - width - SIDEBAR_EDGE_MARGIN;
            let right = b.w - SIDEBAR_EDGE_MARGIN + 2.0;
            let header_y = bottom - height;
            let plate_x = left - 2.0;
            let plate_w = right - plate_x;
            b.rect_px(
                plate_x,
                header_y - 10.0,
                plate_w,
                9.0,
                [0.0, 0.0, 0.0, SIDEBAR_HEADER_BG_ALPHA],
            );
            b.rect_px(
                plate_x,
                header_y - 1.0,
                plate_w,
                bottom - (header_y - 1.0),
                [0.0, 0.0, 0.0, SIDEBAR_BODY_BG_ALPHA],
            );
            let title_x = left + width / 2.0 - title_w / 2.0;
            b.text_spans(&side.title, title_x, header_y - 9.0, sscale, [1.0, 1.0, 1.0], 1.0);
            for (i, l) in side.lines.iter().enumerate() {
                let y = bottom - (entries - i as f32) * SIDEBAR_LINE_H;
                b.text_spans(&l.label, left, y, sscale, [1.0, 1.0, 1.0], 1.0);
                let score_w = b.spans_width(&l.score, sscale);
                b.text_spans(&l.score, right - score_w, y, sscale, SIDEBAR_SCORE_DEFAULT, 1.0);
            }
        }

        // The top-right status-effect overlay — `Hud.extractEffects`, ported
        // rather than approximated.
        //
        // Two blits per effect, both through the GUI atlas: a 24x24 background
        // plate and the effect's own 18x18 `mob_effect/<id>` icon three pixels
        // inside it. **No text at all** — that is the widget, not an omission;
        // the named, timed version this replaced was a stand-in for art that
        // could not be reached from the overlay's own untextured pipeline.
        //
        // The row split is the interesting half. Each row keeps its *own*
        // counter and both start from the right edge, so the first beneficial
        // effect and the first harmful one sit in the same column, one above
        // the other — a single shared counter would stagger them, and reads as
        // plausible until two of one kind and one of the other are active.
        //
        // With no GUI atlas attached `b.sprite` emits nothing, so a jar-less
        // run draws no overlay at all. That matches the inventory column's own
        // choice for the same reason: a coloured rectangle standing in for a
        // missing icon is indistinguishable from art that failed to load.
        if let Some(icons) = frame.effects {
            let mut beneficial = 0u32;
            let mut harmful = 0u32;
            for icon in icons {
                let (n, y) = if icon.beneficial {
                    beneficial += 1;
                    (beneficial, effects::HUD_EFFECT_TOP_Y)
                } else {
                    harmful += 1;
                    (harmful, effects::HUD_EFFECT_TOP_Y + effects::HUD_EFFECT_ROW_DROP)
                };
                let x = b.w - effects::HUD_EFFECT_STRIDE * n as f32;
                // The plate is blitted untinted at full alpha in both branches;
                // only the icon carries vanilla's own ARGB-white helper applied to `alpha`. Fading the plate
                // too is the obvious-looking mistake and makes the whole widget
                // blink rather than the icon inside it.
                b.sprite(
                    icon.background,
                    x,
                    y,
                    effects::HUD_EFFECT_BACKGROUND_SIZE,
                    effects::HUD_EFFECT_BACKGROUND_SIZE,
                    [1.0, 1.0, 1.0, 1.0],
                );
                b.sprite(
                    &icon.icon,
                    x + effects::HUD_EFFECT_ICON_INSET,
                    y + effects::HUD_EFFECT_ICON_INSET,
                    effects::HUD_EFFECT_ICON_SIZE,
                    effects::HUD_EFFECT_ICON_SIZE,
                    [1.0, 1.0, 1.0, icon.alpha],
                );
            }
        }

        // The Tab player-list overlay — `PlayerTabOverlay.extractRenderState`,
        // ported rather than approximated.
        //
        // Read as vanilla's own draw order, because this GUI path has no depth
        // compare and submission order is the only z there is: the header plate
        // and its lines, the row plate, then per row a translucent slot fill, the
        // name, and the ping bars, then the footer plate and its lines.
        //
        // Everything here is at `TAB_TEXT_SCALE`/`TAB_LINE_H` — vanilla's own
        // metrics in the logical canvas — and *not* the HUD's 2× pitch that the
        // rest of `build_inner` uses. See `TAB_LINE_H`.
        if let Some(players) = frame.players {
            let tab_scale = TAB_TEXT_SCALE;
            // The two font measurements vanilla takes, through the same
            // `spans_width`/`text_width` the draw uses. `max_name_width` sizes
            // the column; the banner width only ever *widens* the plates.
            let max_name_width = players
                .rows
                .iter()
                .map(|row| b.spans_width(&row.name, tab_scale))
                .fold(0.0f32, f32::max);
            let widest_banner = players
                .header
                .iter()
                .chain(players.footer.iter())
                .map(|l| b.spans_width(l, tab_scale))
                .fold(0.0f32, f32::max);
            let panel = TabPanel::new(
                b.w,
                players.len(),
                max_name_width,
                players.header.len(),
                widest_banner,
            );
            let plate_x = panel.plate_x();
            let plate_w = panel.plate_w();

            // The header plate spans `yyo - 1 ..= yyo + n * 9`, so it is one
            // pixel taller than the lines it holds. Drawn only when the server
            // actually sent a header: a vanilla server sends none unless
            // something sets one, and fabricating one to fill the space is what
            // this overlay must not do.
            if !players.header.is_empty() {
                b.rect_px(
                    plate_x,
                    panel.header_top - 1.0,
                    plate_w,
                    players.header.len() as f32 * TAB_LINE_H + 1.0,
                    TAB_PLATE,
                );
                for (i, line) in players.header.iter().enumerate() {
                    let x = panel.centred_x(b.spans_width(line, tab_scale));
                    b.text_spans(
                        line,
                        x,
                        panel.header_y(i),
                        tab_scale,
                        [TAB_INK[0], TAB_INK[1], TAB_INK[2]],
                        TAB_INK[3],
                    );
                }
            }

            // The row plate is drawn unconditionally, sized to `rows` — the rows
            // *per column*, not the player count, so a two-column list gets one
            // plate half as tall as a naive `slots * 9` would make it.
            b.rect_px(
                plate_x,
                panel.rows_top - 1.0,
                plate_w,
                panel.rows as f32 * TAB_LINE_H + 1.0,
                TAB_PLATE,
            );

            for (i, row) in players.rows.iter().enumerate() {
                let [sx, sy] = panel.slot_origin(i);
                // `fill(xo, yo, xo + slotWidth, yo + 8, background)` — 8 tall
                // inside a 9 px pitch, which is what leaves the 1 px gap between
                // rows that makes the list read as a list.
                b.rect_px(sx, sy, panel.slot_w, TAB_LINE_H - 1.0, TAB_ROW_FILL);
                let ink = if row.spectator { TAB_INK_SPECTATOR } else { TAB_INK };
                b.text_spans(&row.name, sx, sy, tab_scale, [ink[0], ink[1], ink[2]], ink[3]);
                // The signal bars, right-aligned inside the slot. Vanilla's
                // `extractPingIcon` subtracts the head offset back off `xo`, so
                // the icon is measured from the **slot's** left edge and does not
                // move when a head is drawn.
                b.sprite(
                    row.ping_sprite,
                    sx + panel.slot_w - TAB_PING_INSET,
                    sy,
                    TAB_PING_W,
                    TAB_PING_H,
                    TAB_INK,
                );
            }

            if !players.footer.is_empty() {
                b.rect_px(
                    plate_x,
                    panel.footer_top - 1.0,
                    plate_w,
                    players.footer.len() as f32 * TAB_LINE_H + 1.0,
                    TAB_PLATE,
                );
                for (i, line) in players.footer.iter().enumerate() {
                    let x = panel.centred_x(b.spans_width(line, tab_scale));
                    b.text_spans(
                        line,
                        x,
                        panel.footer_y(i),
                        tab_scale,
                        [TAB_INK[0], TAB_INK[1], TAB_INK[2]],
                        TAB_INK[3],
                    );
                }
            }
        }

        // Sound-subtitle captions, bottom-right.
        if !frame.sound_subtitles.is_empty() {
            draw_sound_subtitles(&mut b, frame.sound_subtitles);
        }

        // Recipe-unlock toast, top-right. Drawn last so it lands
        // over the sidebar/tab overlays, matching vanilla's own toast layer,
        // which `ToastManager.render` composites after the HUD entirely.
        if let Some(toast) = &frame.recipe_toast {
            draw_recipe_toast(&mut b, toast);
        }
        // The advancement-completion toast, same slot and layer.
        if let Some(toast) = &frame.advancement_toast {
            draw_advancement_toast(&mut b, toast);
        }
        if let Some(toast) = &frame.friends_toast {
            draw_friends_toast(&mut b, toast);
        }

        // A chat `hover_event` tooltip, drawn absolutely last — vanilla's own
        // tooltip layer composites over everything, including toasts
        // (`GuiGraphics.renderDeferredTooltip`, called at the very end of
        // `Gui.render`).
        if let Some(tooltip) = &frame.chat_hover_tooltip {
            draw_chat_hover_tooltip(&mut b, tooltip);
        }

        Self {
            verts: b.verts,
            sprite_verts: b.sprite_verts,
            item_verts: b.item_verts,
            glint_verts: b.glint_verts,
            model_verts: b.model_verts,
            special: b.special,
            crosshair: b.crosshair,
        }
    }
}
