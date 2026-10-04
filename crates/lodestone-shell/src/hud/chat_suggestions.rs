use super::*;

/// One composite layer of the suggestion popup, in the order
/// [`SUGGESTION_LAYERS`] lists them.
///
/// Split out because the popup has to sit *over* the chat scrollback and *under*
/// its own tooltip, and a bare call sequence records neither: the ordering is
/// data here, so a reader can see it and a future layer can be inserted at a
/// named position instead of by moving a statement. This is the popup's own
/// order and nothing wider — see [`draw_command_suggestions`]'s doc for what
/// must composite above the whole widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SuggestionLayer {
    /// The per-row translucent fills — `graphics.fill(..., fillColor)`.
    RowFills,
    /// The 1px dotted edges that appear only when the list is scrollable.
    ScrollHints,
    /// The candidate texts, highlighted row in yellow.
    RowTexts,
    /// The hovered candidate's `Message`, when it has one.
    Tooltip,
}

/// The popup's composite order, bottom-most first.
///
/// `RowFills` before `ScrollHints` because the hints draw a `fillColor` band in
/// the 1px gutters *outside* the rows and then stipple white over it, so a row
/// fill submitted afterwards would paint over the stipple where they abut.
pub(super) const SUGGESTION_LAYERS: [SuggestionLayer; 4] = [
    SuggestionLayer::RowFills,
    SuggestionLayer::ScrollHints,
    SuggestionLayer::RowTexts,
    SuggestionLayer::Tooltip,
];

/// Draw the command-suggestion dropdown — `SuggestionsList.extractRenderState`.
///
/// # What must composite above this, and why the call site is where it is
///
/// The popup is submitted **after** the chat input line and the whole
/// scrollback, because it overlaps both. Everything that must still sit on top
/// of it, in the order it draws:
///
/// | above the popup | why |
/// |---|---|
/// | this widget's own tooltip | [`SuggestionLayer::Tooltip`], last in [`SUGGESTION_LAYERS`] |
/// | the F3 debug overlay | vanilla draws `DebugScreenOverlay` after every screen |
/// | a container screen's cursor stack and item tooltip | separate pass, separate geometry type — see `container.rs` |
///
/// Only the first is this function's business; the other two composite in later
/// passes and need nothing from here. **The F3 overlay is currently submitted
/// *first* in [`HudGeometry::build_inner`] and therefore draws underneath**,
/// which is a pre-existing divergence for the whole HUD rather than one this
/// widget introduces — named here because this is the table a reader will check.
pub(super) fn draw_command_suggestions(
    b: &mut Builder,
    popup: &SuggestionPopup<'_>,
    layout: SuggestionLayout,
    pose_scale: f32,
    layers: &[SuggestionLayer],
) {
    if layout.rows == 0 {
        return;
    }
    let px = pose_scale.max(1.0);
    let has_previous = popup.offset > 0;
    let has_next = popup.candidates.len() > popup.offset + layout.rows;
    for layer in layers {
        match layer {
            SuggestionLayer::RowFills => {
                for i in 0..layout.rows {
                    b.rect_px(
                        layout.x,
                        layout.y + layout.row_h * i as f32,
                        layout.w,
                        layout.row_h,
                        SUGGESTION_FILL,
                    );
                }
            }
            // Vanilla draws a `fillColor` band in the 1px gutter above *and*
            // below whenever the list is scrollable **either** way, then
            // stipples white into whichever end has more rows behind it —
            // `if (limited)` covers both bands, and the two `if`s inside it
            // cover one each. The asymmetry is deliberate: the band alone is
            // what makes the box look clipped rather than ended.
            SuggestionLayer::ScrollHints if has_previous || has_next => {
                b.rect_px(layout.x, layout.y - px, layout.w, px, SUGGESTION_FILL);
                b.rect_px(layout.x, layout.y + layout.h, layout.w, px, SUGGESTION_FILL);
                let white = [1.0, 1.0, 1.0, 1.0];
                // `for (x = 0; x < width; x++) if (x % 2 == 0)` — every other
                // pixel column, so the stipple pitch scales with the box.
                // `x + px <= w`, not `x < w`: the loop variable is the dash's
                // *left* edge and the dash is `px` wide, so the plain `x < w`
                // form emits a final dash that hangs `px` past the panel's own
                // right edge. Invisible at chat scale 1 against a one-pixel
                // tolerance, and the reason the popup's own geometry could sit
                // outside the rect the hit-test uses for it.
                let mut x = 0.0;
                while x + px <= layout.w {
                    if has_previous {
                        b.rect_px(layout.x + x, layout.y - px, px, px, white);
                    }
                    if has_next {
                        b.rect_px(layout.x + x, layout.y + layout.h, px, px, white);
                    }
                    x += 2.0 * px;
                }
            }
            SuggestionLayer::ScrollHints => {}
            SuggestionLayer::RowTexts => {
                for i in 0..layout.rows {
                    let Some(candidate) = popup.candidates.get(popup.offset + i) else {
                        continue;
                    };
                    let colour = if popup.offset + i == popup.selected {
                        SUGGESTION_TEXT_SELECTED
                    } else {
                        SUGGESTION_TEXT_UNSELECTED
                    };
                    b.text(
                        &candidate.text,
                        layout.x + SUGGESTION_TEXT_INSET * pose_scale,
                        layout.y + layout.row_h * i as f32 + SUGGESTION_ROW_PAD_TOP * pose_scale,
                        pose_scale,
                        colour,
                    );
                }
            }
            // `graphics.setTooltipForNextFrame(font, fromMessage(tooltip),
            // mouseX, mouseY)`, gated on `hovered` — so it is the *pointer*
            // that reveals a tooltip, never the keyboard selection, and it
            // shows the **selected** row's message rather than the hovered
            // one's (they are the same row whenever the pointer moved, which is
            // the only way `hovered` becomes true with a stale selection).
            //
            // Only the placement and the text are ported. Vanilla's
            // `TooltipRenderUtil` border gradient is not modelled; this is a
            // flat panel, and that is a cosmetic narrowing rather than a
            // behavioural one.
            SuggestionLayer::Tooltip => {
                let Some((mx, my)) = popup.cursor else {
                    continue;
                };
                if !layout.contains(mx, my) {
                    continue;
                }
                // Via `to_spans`, not `to_legacy_string`: a server-sent
                // tooltip can carry a hex colour (`TextColor::Rgb`), which
                // the legacy flatten has no code for and would silently drop
                // — see `CommandSuggestionEntry::tooltip`'s own doc.
                let Some(spans) = popup
                    .candidates
                    .get(popup.selected)
                    .and_then(|c| c.tooltip.as_ref())
                    .map(lodestone_model::ResolvedText::to_spans)
                else {
                    continue;
                };
                let pad = 3.0 * pose_scale;
                let tw = b.spans_width(&spans, pose_scale);
                let th = font::GLYPH_H as f32 * pose_scale;
                // `renderTooltip`'s own offset from the cursor.
                let tx = (mx + 12.0 * pose_scale).min((b.w - tw - pad * 2.0).max(0.0));
                let ty = (my - 12.0 * pose_scale).max(0.0);
                b.rect_px(tx, ty, tw + pad * 2.0, th + pad * 2.0, SUGGESTION_FILL);
                b.text_spans(&spans, tx + pad, ty + pad, pose_scale, [1.0, 1.0, 1.0], 1.0);
            }
        }
    }
}
