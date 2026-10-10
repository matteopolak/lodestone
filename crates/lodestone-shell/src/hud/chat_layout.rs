use super::*;

/// Vanilla's Chat Settings (plus one Accessibility-screen field it shares)
/// values that shape how the scrollback and input line draw —
/// vanilla's own options type's `chat*` fields.
/// `Copy` for the same reason [`crate::config::Options`] is: cheap to read
/// once per frame with no borrow to fight.
///
/// Deliberately **not** every vanilla chat option: `chatVisibility` (System/
/// Hidden filtering) and `chatDelay` both live upstream of this draw layer —
/// the first needs a per-line message-source tag `ChatLog::recent` currently
/// flattens away, the second is a message-arrival rate limit, not a render
/// concern. Landing an option field with no reader is the exact defect this
/// repo's own `CLAUDE.md` calls the dominant one, so those stay out until
/// something upstream can actually consume them.
///
/// `chatColors`' link-adjacent siblings `chatLinks`/`chatLinksPrompt` used to
/// belong on this list too — click detection landed
/// ([`chat_interaction_at`]/[`HudRenderer::chat_interaction_at`]) — but the
/// *option* still has no reader: there is no confirmation-screen state yet to
/// gate an `open_url` click on (see `docs/chat.md`'s "Interactivity" section
/// for the exact boundary), so the toggle that would control it still has
/// nothing to control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatDisplayOptions {
    /// `options.chat.scale`, `0.0..=1.0` — vanilla's
    /// The chat component's get scale. This is the *entire* pose scale every chat
    /// draw multiplies by ([`chat_pose_scale`]); there is no HUD-side factor
    /// layered on top, matching chat component's extract render state's
    /// `pose.scale(scale, scale)`.
    pub scale: f32,
    /// `options.chat.width`, `0.0..=1.0`. Fed
    /// through [`chat_width_px`] (vanilla's chat component's get width,
    /// vanilla's own chat-component rendering) to size the chat box.
    pub width_pct: f32,
    /// `options.chat.height.unfocused`, `0.0..=1.0`
    /// — box height while the chat box is **closed**.
    pub height_pct_unfocused: f32,
    /// `options.chat.height.focused`, `0.0..=1.0` —
    /// box height while the chat box is **open**.
    pub height_pct_focused: f32,
    /// `options.chat.line_spacing`, `0.0..=1.0`:
    /// extra fraction of a line's height inserted between chat rows
    /// (vanilla's own chat-component rendering, `entryHeight = messageHeight * (spacing +
    /// 1.0)`).
    pub line_spacing: f32,
    /// `options.chat.opacity`, `0.0..=1.0`. Text
    /// alpha is `text_opacity * 0.9 + 0.1` — never
    /// fully transparent, matching vanilla.
    pub text_opacity: f32,
    /// `options.accessibility.text_background_opacity`,
    /// `0.0..=1.0`. Used directly as the per-line background fill alpha.
    pub background_opacity: f32,
    /// `options.chat.color`. `false` strips every
    /// legacy `§` code before drawing a scrollback line
    /// (the component render utils's strip color, vanilla's own component-render-utils helper) —
    /// it never touches the input line, which cannot carry codes
    /// ([`crate::chat::ChatInput::push_char`] filters `§` on the way in).
    pub colors: bool,
}

impl Default for ChatDisplayOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            width_pct: 1.0,
            height_pct_unfocused: 70.0 / 160.0,
            height_pct_focused: 1.0,
            line_spacing: 0.0,
            text_opacity: 1.0,
            background_opacity: 0.5,
            colors: true,
        }
    }
}

/// Vanilla's chat component's get width: maps the
/// `0.0..=1.0` `chatWidth` option onto `40.0..=320.0` **screen** pixels — the
/// same logical-canvas unit [`crate::menu::render::logical_canvas`] returns
/// (see [`HudGeometry::build_inner`]'s own doc on why that canvas *is*
/// vanilla's own scaled GUI width/height), so this is directly comparable to
/// `Builder::w` with no further conversion.
#[must_use]
pub fn chat_width_px(pct: f32) -> f32 {
    (pct * 280.0 + 40.0).floor()
}

/// As [`chat_width_px`], vanilla's chat component's get height
///: maps `0.0..=1.0` onto `20.0..=180.0` screen
/// pixels.
#[must_use]
pub fn chat_height_px(pct: f32) -> f32 {
    (pct * 160.0 + 20.0).floor()
}

/// The scale factor every chat draw — scrollback, input line, suggestion popup
/// — multiplies its geometry by: vanilla's own chat-scale option alone,
/// matching vanilla's own chat-widget render-state extraction, which applies
/// that scale uniformly to both axes with no
/// further HUD-side factor.
///
/// A free function rather than a local `let` because
/// [`suggestion_layout`] is called from outside [`HudGeometry::build_inner`]
/// (the pointer hit-test) and the two must resolve the same number.
#[must_use]
pub fn chat_pose_scale(opts: ChatDisplayOptions) -> f32 {
    opts.scale.max(0.0)
}

/// The top of the chat input line's glyph row, in logical-canvas pixels —
/// vanilla's own text-input widget, at twelve pixels above its own bottom edge.
///
/// Shared by the input draw and [`suggestion_layout`] for the reason that whole
/// function exists: the popup is placed *relative to this line*, so a second
/// spelling of it would let the two drift apart by exactly the amount nobody
/// notices in a screenshot.
#[must_use]
pub fn chat_input_top(canvas_h: f32, pose_scale: f32) -> f32 {
    canvas_h - HUD_MARGIN - font::GLYPH_H as f32 * pose_scale
}

/// The scrollback's own anchor — vanilla's chat component's extract render state
///: `final int chatBottom = Mth.floor((screenHeight -
/// 40) / scale);`, computed in the pose's *local* (unscaled-by-chat-scale)
/// coordinates and then carried back to screen/canvas pixels by the very
/// `pose.scale(scale, scale)` that local value is drawn under — so the real
/// canvas-pixel anchor is `floor((canvas_h - 40) / scale) * scale`. Every
/// message row's bottom edge is `chatBottom - lineIndex * entryHeight`
/// (`lineIndex == 0` for the newest), so this is where the newest row's
/// bottom edge lands.
///
/// **Independent of the input box.** `extractRenderState` computes this one
/// expression before it ever branches on `displayMode.foreground`, and the
/// `EditBox` (`this.height - 12`, the chat screen's init) is a wholly separate
/// literal in a different class — vanilla never derives one from the other.
/// So this takes `canvas_h` and the chat scale only, not [`chat_input_top`]:
/// coupling the two (as this HUD used to, computing `chat_bottom` from
/// `input_y` while the box was open) is what silently made the vanilla gap
/// disappear, since that coupling forces the newest row flush against the
/// input strip's own top edge regardless of what `40` says. Used unconditionally,
/// open or closed, for the same reason — vanilla's own chat-bottom coordinate does not
/// change with the chat's own open/closed display mode either.
///
/// At the vanilla-default chat-scale option of `1.0` this is simply `canvas_h -
/// 40.0`: a fixed, real 40 logical-canvas-pixel headroom above wherever the
/// input box happens to sit, not a restated `0`.
#[must_use]
pub fn chat_bottom(canvas_h: f32, pose_scale: f32) -> f32 {
    if pose_scale <= 0.0 {
        return canvas_h - 40.0;
    }
    ((canvas_h - 40.0) / pose_scale).floor() * pose_scale
}

/// One scrollback row's vertical pitch — vanilla's `entryHeight = messageHeight
/// * (lineSpacing + 1.0)`, scaled by the chat pose
/// afterward. `messageHeight` is vanilla's literal `9`; `glyph_h + 2.0` is this
/// HUD's own 5×7-font analogue of it.
///
/// A free function, not an inline `let`, so a caller outside the draw (the
/// mouse-wheel scroll handler and the per-frame scroll sync, neither of which
/// build a full [`HudFrame`]) can compute the same "how many rows fit the
/// chat box" figure the draw itself uses, without restating the formula.
#[must_use]
pub fn chat_line_h(opts: ChatDisplayOptions, pose_scale: f32) -> f32 {
    (font::GLYPH_H as f32 + 2.0) * (1.0 + opts.line_spacing.max(0.0)) * pose_scale
}

/// How many scrollback rows fit the configured chat box height — vanilla's
/// The chat component's get lines per page (`height / lineHeight`, vanilla's own chat-component rendering),
/// at this crate's entry-granularity approximation of a "row" (see
/// [`crate::chat::ChatScroll`]'s own doc). Shared by the draw and by
/// [`crate::chat::ChatScroll::scroll`]'s callers so a resize cannot leave the
/// two disagreeing about how many lines the box holds.
#[must_use]
pub fn chat_lines_per_page(opts: ChatDisplayOptions, pose_scale: f32, chat_open: bool) -> usize {
    let height_pct = if chat_open {
        opts.height_pct_focused
    } else {
        opts.height_pct_unfocused
    };
    let box_h = chat_height_px(height_pct.clamp(0.0, 1.0));
    let line_h = chat_line_h(opts, pose_scale);
    (box_h / line_h).floor().max(1.0) as usize
}

/// Vanilla's command suggestions's line height is `12`, decomposed: the 9px font
/// draws at `rect.getY() + 2 + 12 * i`, so the row is 2px of lead, the glyph,
/// and 1px of trail. Ours keeps the padding and substitutes this HUD's own glyph
/// height, rather than restating `12` against a 7px font.
pub(super) const SUGGESTION_ROW_PAD_TOP: f32 = 2.0;
/// See [`SUGGESTION_ROW_PAD_TOP`] — `12 - 2 - 9`.
pub(super) const SUGGESTION_ROW_PAD_BOTTOM: f32 = 1.0;
/// The gap between the popup's bottom edge and the input line —
/// `SuggestionsList`'s `y - 3 - rows * 12` when `anchorToBottom` is set, which
/// The chat screen's init does.
pub(super) const SUGGESTION_LIST_GAP: f32 = 3.0;
/// The 1px left inset the row text draws at (`rect.getX() + 1`), which is also
/// why the rect is `maxWidth + 1` wide and starts one pixel left of the anchor
/// (`listX = x - 1` for an unbordered `EditBox`, and the chat screen sets
/// set bordered).
pub(super) const SUGGESTION_TEXT_INSET: f32 = 1.0;

/// The command suggestions's fill color as the chat screen's init passes it:
/// `-805306368` == `0xD0000000`.
pub(super) const SUGGESTION_FILL: [f32; 4] = [0.0, 0.0, 0.0, 208.0 / 255.0];
/// The highlighted row's text colour — `-256` == `0xFFFFFF00`.
pub(super) const SUGGESTION_TEXT_SELECTED: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
/// Every other row's text colour — `-5592406` == `0xFFAAAAAA`.
pub(super) const SUGGESTION_TEXT_UNSELECTED: [f32; 4] = [170.0 / 255.0, 170.0 / 255.0, 170.0 / 255.0, 1.0];
/// `EditBox.extractRenderState`'s ghost-suffix colour — `-8355712` ==
/// `0xFF808080`, drawn at `cursorX - 1`.
pub(super) const SUGGESTION_GHOST: [f32; 4] = [0.5019608, 0.5019608, 0.5019608, 1.0];
/// The gui graphics extractor's text highlight's opaque blue selection pass. Vanilla
/// also inverts its glyphs through a dedicated GUI pipeline; the colour stream
/// has no equivalent pipeline, so this pass remains behind the white glyphs.
pub(super) const CHAT_SELECTION: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// Pixel width of `s` at `scale`, in whichever font is attached — the real
/// vanilla proportional advances when there is one, the fixed 5×7 debug advance
/// otherwise.
///
/// Free-standing rather than a `Builder` method so the pointer hit-test can
/// measure identically to the draw ([`HudRenderer::suggestion_layout`]);
/// `Builder::text_width` is a thin wrapper over it, which is what makes that
/// identity structural rather than two copies of one `match`.
pub(super) fn measure_text(font: Option<&VanillaFont>, s: &str, scale: f32) -> f32 {
    match font {
        Some(f) => f.width(s, scale),
        None => item_icon::text_w(s, scale),
    }
}

/// What [`HudFrame::chat_scrollbar`] needs to draw vanilla's own scrollback
/// indicator — the three numbers [`crate::chat::ChatScroll`] already tracks,
/// read straight off it rather than re-derived at the draw site.
#[derive(Debug, Clone, Copy)]
pub struct ChatScrollbar {
    /// [`crate::chat::ChatScroll::scrolled`] — entries scrolled back from the
    /// newest.
    pub scrolled: usize,
    /// The full history length the scroll position is relative to —
    /// vanilla's `trimmedMessages.size()`, at this crate's entry granularity.
    pub total: usize,
    /// [`crate::chat::ChatScroll::new_message_since_scroll`] — tints the bar
    /// vanilla's amber (`0xCC3333`) instead of its usual blue-grey
    /// (`0x3333AA`).
    pub new_message_since_scroll: bool,
}

/// What the suggestion popup needs from `chat::SuggestionsList` to lay itself
/// out and draw. Built by the caller each frame; `None` on [`HudFrame`] is "no
/// popup", which is the only gate the draw has.
#[derive(Debug, Clone, Copy)]
pub struct SuggestionPopup<'a> {
    /// The input line the candidates replace a tail of — needed because the
    /// popup's x anchor is the pixel `line[..start]` ends at, vanilla's
    /// `input.get_screen_x(suggestions.get_range().get_start())`.
    pub line: &'a str,
    /// Byte offset into [`Self::line`] the candidate text replaces from.
    pub start: usize,
    /// Every candidate, in row order.
    pub candidates: &'a [crate::chat::Candidate],
    /// The highlighted row's index into [`Self::candidates`].
    pub selected: usize,
    /// The first *visible* row's index into [`Self::candidates`].
    pub offset: usize,
    /// The pointer, in logical-canvas pixels, when it is over the window.
    ///
    /// Used for the tooltip only. Hover *selection* is a state change and
    /// belongs to the event loop, which resolves the row through
    /// [`SuggestionLayout::row_at`] against this same layout.
    pub cursor: Option<(f32, f32)>,
}

/// What [`HudFrame::chat_hover_tooltip`] needs to draw a hover tooltip near
/// the cursor.
///
/// All three hover actions are drawn through this one field, because all
/// three end up as the same thing: a stack of styled lines in a box.
/// [`hover_tooltip_spans`] is what turns each payload into those lines —
/// `show_text`'s component directly, `show_item`'s stack through the same
/// line-gathering an inventory slot's tooltip uses, and `show_entity`'s three
/// parts into its three lines. What is *not* reproduced is the item tooltip's
/// non-text furniture: no bundle grid, no icon, no nine-slice sprite frame,
/// since this box is painted from the HUD's untextured colour stream.
#[derive(Debug, Clone, Copy)]
pub struct ChatHoverTooltip<'a> {
    /// The tooltip body, via [`lodestone_model::Text::to_spans`] so a
    /// hex-coloured `show_text` hover keeps its real colour — a plain
    /// [`lodestone_model::Text::to_legacy_string`] flatten can only represent
    /// the sixteen legacy codes. May contain a literal `\n` — chat hover
    /// payloads can be multi-line (an enchanted book's enchantment list, for
    /// instance), and this is split on it before word-wrap.
    pub spans: &'a [TextSpan],
    /// The pointer, in logical-canvas pixels — [`HudRenderer::canvas_cursor`]'s
    /// own output, the same anchor [`SuggestionPopup::cursor`] uses.
    pub cursor: (f32, f32),
}

/// The popup's resolved rect — vanilla's `SuggestionsList.rect`, plus the row
/// pitch a hit-test needs.
///
/// Vanilla computes this **once**, in `showSuggestions`, and every later mouse
/// event tests the stored value. Here it is recomputed from the same expression
/// the draw uses, which is strictly closer to the pixels: a resize between the
/// show and the click cannot leave the hit-test aiming at a stale rect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SuggestionLayout {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width, including the 1px text inset.
    pub w: f32,
    /// Height — `rows * row_h`.
    pub h: f32,
    /// One row's pitch, vanilla's `LINE_HEIGHT` at this frame's chat scale.
    pub row_h: f32,
    /// How many rows are visible — `min(candidates, SUGGESTION_LINE_LIMIT)`.
    pub rows: usize,
}

impl SuggestionLayout {
    /// Whether `(x, y)` is inside the rect — `Rect2i.contains`.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// The candidate index under `(x, y)`, or `None` outside the rect —
    /// `SuggestionsList.mouseClicked`'s `(y - rect.getY()) / 12 + offset`,
    /// guarded by that method's own `line < suggestionList.size()`.
    ///
    /// `offset` is the caller's, not stored here, because the layout is rebuilt
    /// every frame while the window position is list state.
    ///
    /// **One named narrowing.** Vanilla's *hover* test is one pixel stricter
    /// than its *click* test on every edge (`mouseX > rect.getX()` versus
    /// `Rect2i.contains`'s `>=`), so in vanilla the outermost pixel ring
    /// click-selects but does not hover-select. Both go through this method
    /// here; the difference is one pixel of hover on a row you can still click.
    #[must_use]
    pub fn row_at(&self, x: f32, y: f32, offset: usize, candidates: usize) -> Option<usize> {
        if !self.contains(x, y) || self.row_h <= 0.0 {
            return None;
        }
        let row = ((y - self.y) / self.row_h).floor().max(0.0) as usize + offset;
        (row < candidates).then_some(row)
    }
}

/// Lay the popup out — the command suggestions's show suggestions plus
/// `SuggestionsList`'s constructor, against this HUD's own chat geometry.
///
/// `text_width` measures at the frame's chat pose scale, i.e. it is exactly what
/// the draw will use to place glyphs; passing anything else is how a rect ends
/// up describing a box the text does not fit.
///
/// `canvas_w`/`canvas_h` are logical-canvas pixels — the
/// `crate::menu::render::logical_canvas` space vanilla calls
/// `guiScaledWidth`/`guiScaledHeight`.
#[must_use]
pub fn suggestion_layout(
    canvas_w: f32,
    canvas_h: f32,
    pose_scale: f32,
    popup: &SuggestionPopup<'_>,
    text_width: impl Fn(&str) -> f32,
) -> SuggestionLayout {
    let rows = popup.candidates.len().min(crate::chat::SUGGESTION_LINE_LIMIT);
    let max_w = popup
        .candidates
        .iter()
        .map(|c| text_width(&c.text))
        .fold(0.0_f32, f32::max);
    let row_h = (SUGGESTION_ROW_PAD_TOP + font::GLYPH_H as f32 + SUGGESTION_ROW_PAD_BOTTOM)
        * pose_scale;
    // `input.get_screen_x(range.get_start())` — the pixel the replaced span starts
    // at, measured through the *same* metrics the line was drawn with. The
    // `min(start, len)` is defensive against a server-supplied `start`; the
    // char-boundary case cannot reach here because `ChatCompletion::show`
    // rejects it.
    let head_end = popup.start.min(popup.line.len());
    // The origin is the input line's own first glyph — [`CHAT_TEXT_INSET`],
    // scaled, the same expression the draw uses. It read `HUD_MARGIN` while the
    // comment below already said "the input is at x=4", which is how the popup
    // came to hang two pixels right of the token it was completing.
    let anchor =
        CHAT_TEXT_INSET * pose_scale + text_width(popup.line.get(..head_end).unwrap_or(""));
    // Vanilla clamps to `0 ..= get_screen_x(0) + innerWidth - maxWidth`, and for
    // the chat box that collapses to `screenWidth - maxWidth`: the input is at
    // x=4 with `innerWidth == width - 4`. So this is "do not run off the right
    // edge", not a chat-box-width clamp — the popup is a `Screen` widget and is
    // not bound by the `chatWidth` option.
    let x = anchor.clamp(0.0, (canvas_w - max_w).max(0.0)) - SUGGESTION_TEXT_INSET * pose_scale;
    let bottom = chat_input_top(canvas_h, pose_scale) - SUGGESTION_LIST_GAP * pose_scale;
    SuggestionLayout {
        x,
        y: bottom - rows as f32 * row_h,
        w: max_w + SUGGESTION_TEXT_INSET * pose_scale,
        h: rows as f32 * row_h,
        row_h,
        rows,
    }
}
