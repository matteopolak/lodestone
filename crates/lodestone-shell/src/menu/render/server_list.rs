//! Vanilla's `JoinMultiplayerScreen`/`ServerSelectionList` metrics, its
//! header/footer layout block, and the server-list row, icon and scroll
//! geometry.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

// -- vanilla's `JoinMultiplayerScreen` / `ServerSelectionList` metrics --------
//
// Every number below is from `.cache/mc/26.2/client-src/net/minecraft/client/gui/
// screens/multiplayer/`, with the line named. Deliberately its own set of
// constants rather than shared with the world-select block above: the two
// screens agree on several values *by coincidence* (both list `itemHeight`s are
// 36, both content paddings are 2 because they inherit the same base class), and
// a shared constant would make a divergence in one screen silently move the
// other.

/// `new HeaderAndFooterLayout(this, 33, 60)` (`JoinMultiplayerScreen.java`) —
/// the header band. This is the default 33 spelled out, not
/// [`layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT`], because the *footer* is not the
/// default and the pair is one constructor call.
const SERVER_LIST_HEADER_H: f32 = 33.0;
/// The same call's footer band: 60, because this screen's footer is two rows of
/// buttons rather than one.
pub(super) const SERVER_LIST_FOOTER_H: f32 = 60.0;
/// `LinearLayout.vertical().spacing(4)` and both
/// `LinearLayout.horizontal().spacing(4)` rows (`:64,66,67`).
const SERVER_LIST_FOOTER_SPACING: i32 = 4;
/// `JoinMultiplayerScreen.TOP_ROW_BUTTON_WIDTH` (`:28`) — Join Server / Direct
/// Connection / Add Server.
const SERVER_LIST_TOP_BUTTON_W: f32 = 100.0;
/// `JoinMultiplayerScreen.LOWER_ROW_BUTTON_WIDTH` (`:29`) — Edit / Delete /
/// Refresh / Back.
const SERVER_LIST_LOWER_BUTTON_W: f32 = 74.0;
/// The `itemHeight` the list is constructed with: the last argument of
/// `new ServerSelectionList(…, 36)` (`:61-62`).
///
/// **Public because `MenuNav::scroll_server_to_show` needs it**:
/// with the scroll offset in pixels, the keyboard scroll-into-view path has to
/// turn a row index into a pixel top, and a second copy of `36.0` in `nav.rs`
/// is exactly how the draw and the hit-test drift apart.
pub const SERVER_LIST_ITEM_H: f32 = 36.0;
/// `ServerSelectionList.getRowWidth()` (`ServerSelectionList.java`) — a
/// 305 px override of `AbstractSelectionList`'s 220.
pub(super) const SERVER_LIST_ROW_W: f32 = 305.0;
/// `AbstractSelectionList.Entry.CONTENT_PADDING` (`AbstractSelectionList.java`).
/// The entry rect is inset by this on each side, so a 36 px row has a **32** px
/// content box — exactly [`SERVER_ENTRY_ICON`], which is why the favicon fills
/// the row's height.
const SERVER_LIST_ENTRY_PADDING: f32 = 2.0;
/// `getFirstEntryY() = getY() + 2` (`AbstractSelectionList.java`): the
/// gap above row 0. A different expression from [`SERVER_LIST_ENTRY_PADDING`]
/// that happens to be the same 2 — only one of them insets a row.
const SERVER_LIST_FIRST_ENTRY_Y: f32 = 2.0;
/// `OnlineServerEntry.ICON_SIZE` (`ServerSelectionList.java`).
pub(super) const SERVER_ENTRY_ICON: f32 = 32.0;
/// `OnlineServerEntry.SPACING` (`:247`) — the gap the status icon and the status
/// text keep from the content's right edge, and from each other.
pub(super) const SERVER_ENTRY_SPACING: f32 = 5.0;
/// `OnlineServerEntry.STATUS_ICON_WIDTH` (`:248`).
const SERVER_STATUS_ICON_W: f32 = 10.0;
/// `OnlineServerEntry.STATUS_ICON_HEIGHT` (`:249`).
const SERVER_STATUS_ICON_H: f32 = 8.0;
/// The gap between the favicon and the name/MOTD column: vanilla writes
/// `getContentX() + 32 + 3` (`:306,310`) — a literal 3, *not*
/// [`SERVER_ENTRY_SPACING`]'s 5.
pub(super) const SERVER_ENTRY_TEXT_GAP: f32 = 3.0;
/// The first MOTD line's offset below the content's top: `getContentY() + 12`
/// (`:310`). Subsequent lines step by [`LINE_H`] (`+ 9 * i`).
pub(super) const SERVER_ENTRY_MOTD_Y: f32 = 12.0;
/// How many MOTD lines a row shows — `Math.min(lines.size(), 2)` (`:309`).
pub(super) const SERVER_ENTRY_MOTD_LINES: usize = 2;
/// The width the MOTD wraps to: `getContentWidth() - 32 - 2` (`:307`). The 2 is
/// its own literal, not the content padding.
pub(super) const SERVER_ENTRY_MOTD_INSET: f32 = SERVER_ENTRY_ICON + 2.0;
/// A `StringWidget`'s height, which is what the title header is
/// (`StringWidget.java`, `HeaderAndFooterLayout.addTitleHeader`).
const SERVER_LIST_TITLE_H: f32 = 9.0;

/// The MOTD and status colour, `-8355712` (`ServerSelectionList.java`).
/// A mid grey — `0xFF808080`.
pub(super) const SERVER_ENTRY_DIM: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// `CANT_RESOLVE_TEXT`/`CANT_CONNECT_TEXT`'s `withColor(-65536)` (`:68-69`) —
/// pure red, and a *component* colour, so it overrides the `-8355712` the MOTD
/// line is otherwise drawn with.
pub(super) const SERVER_ENTRY_BAD: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
/// `ChatFormatting.RED`, `0xFF5555` — the version string an incompatible row
/// shows where a compatible one shows its player count (`:344-346`).
pub(super) const SERVER_ENTRY_INCOMPATIBLE: [f32; 4] = [1.0, 85.0 / 255.0, 85.0 / 255.0, 1.0];
/// The selected row's interior, `-16777216` — opaque black, filled inside the
/// 1 px outline (`AbstractSelectionList.java`).
pub(super) const SERVER_LIST_SELECTION_FILL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// The hovered row's icon dim, `fill(…, -1601138544)` (`ServerSelectionList.java`)
/// — `0xA0909090`, a translucent grey *over* the favicon, which is what makes
/// the join/move arrows on top of it readable.
pub(super) const SERVER_ICON_DARKEN: [f32; 4] = [144.0 / 255.0, 144.0 / 255.0, 144.0 / 255.0, 160.0 / 255.0];

/// `ServerSelectionList.JOIN_SPRITE` and its highlighted twin (`:52-53`).
pub(super) const SERVER_JOIN_SPRITES: (&str, &str) = ("server_list/join", "server_list/join_highlighted");
/// `MOVE_UP_SPRITE` / `MOVE_UP_HIGHLIGHTED_SPRITE` (`:54-55`).
pub(super) const SERVER_MOVE_UP_SPRITES: (&str, &str) =
    ("server_list/move_up", "server_list/move_up_highlighted");
/// `MOVE_DOWN_SPRITE` / `MOVE_DOWN_HIGHLIGHTED_SPRITE` (`:56-57`).
pub(super) const SERVER_MOVE_DOWN_SPRITES: (&str, &str) =
    ("server_list/move_down", "server_list/move_down_highlighted");
/// `FaviconTexture.MISSING_LOCATION`, blitted for a row whose server sent no
/// usable icon. A **loose** texture, so it reaches the atlas through
/// [`crate::resources::UNKNOWN_SERVER_TEXTURE`] rather than the sprite glob.
///
/// **Not only this screen's.** `FaviconTexture` is also what `WorldListEntry`
/// holds its thumbnail in, so the same file is the world list's missing-icon
/// fallback — see [`WORLD_UNKNOWN_ICON`], which aliases this rather than
/// restating the path.
pub(super) const SERVER_UNKNOWN_ICON: &str = "misc/unknown_server";

/// The world list's missing-thumbnail fallback, which vanilla shares with the
/// server list because both rows hold a `FaviconTexture` and its
/// `MISSING_LOCATION` is one file.
///
/// An alias rather than a second constant so the shared-ness is stated in the
/// source; it is re-exported under its own name because a reader of
/// `draw_world_entry` should not have to know that the world thumbnail is a
/// *server* texture to follow the code.
pub(super) const WORLD_UNKNOWN_ICON: &str = SERVER_UNKNOWN_ICON;

/// Vanilla's `JoinMultiplayerScreen.init` (`JoinMultiplayerScreen.java`)
/// as a real [`layout::HeaderAndFooterLayout`], arranged for a `width`×`height`
/// canvas.
///
/// Two notes before changing it:
///
/// - **The title cell is zero-width**, for the reason `world_select_layout`
///   gives: `addTitleHeader` adds a `StringWidget(title, font)` and there is no
///   font at arrange time, but the header frame centres its child, so a
///   zero-width cell lands exactly on the centre a real-width one would be
///   centred about.
/// - **The list is a [`layout::SpacerElement`]**, sized to
///   `layout.getContentHeight()` exactly as `:61-62` does. It has to take part in
///   the measurement — `HeaderAndFooterLayout`'s content clamp reads the content
///   frame's height — and a spacer is measured and never drawn, which is right:
///   the list draws through [`draw_server_entry`], not as a widget.
fn server_list_layout(width: f32, height: f32) -> layout::HeaderAndFooterLayout {
    let button = |w: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, WIDGET_H, ""))
    };
    let mut root = layout::HeaderAndFooterLayout::with_heights(
        width,
        height,
        SERVER_LIST_HEADER_H,
        SERVER_LIST_FOOTER_H,
    );

    // `this.layout.addTitleHeader(this.title, this.font)` (`:49`).
    root.add_to_header(Box::new(Widget::new(
        0.0,
        0.0,
        0.0,
        SERVER_LIST_TITLE_H,
        super::nav::SERVER_LIST_TITLE,
    )));

    let content_height = root.content_height();
    root.add_to_contents(Box::new(layout::SpacerElement::new(width, content_height)));

    // `LinearLayout footer = this.layout.addToFooter(LinearLayout.vertical().spacing(4));`
    // `footer.defaultCellSetting().alignHorizontallyCenter();` (`:64-65`) — the
    // *live* baseline, so both rows inherit the centring.
    let mut footer = layout::LinearLayout::vertical().spacing(SERVER_LIST_FOOTER_SPACING);
    {
        let baseline = footer.default_cell_setting();
        *baseline = baseline.align_horizontally_center();
    }
    let mut top = layout::LinearLayout::horizontal().spacing(SERVER_LIST_FOOTER_SPACING);
    for _ in 0..3 {
        top.add_child(button(SERVER_LIST_TOP_BUTTON_W));
    }
    footer.add_child(Box::new(top));
    let mut bottom = layout::LinearLayout::horizontal().spacing(SERVER_LIST_FOOTER_SPACING);
    for _ in 0..4 {
        bottom.add_child(button(SERVER_LIST_LOWER_BUTTON_W));
    }
    footer.add_child(Box::new(bottom));
    root.add_to_footer(Box::new(footer));

    root.arrange_elements();
    root
}

/// One arranged multiplayer screen: the title cell, the seven footer buttons,
/// and where the content band starts.
///
/// Same shape and same reason as `WorldSelectBlock`: the two bands are anchored
/// to *different* [`Origin`]s, so a flat list of absolute rects could not be
/// turned back into canvas-independent offsets.
#[derive(Debug)]
pub(super) struct ServerListBlock {
    /// The header's one leaf — the title cell.
    title: (f32, f32, f32, f32),
    /// The footer's leaves, in [`super::nav::SERVER_LIST_BUTTONS`]' order.
    pub(super) footer: Vec<(f32, f32, f32, f32)>,
    /// The content frame's top, i.e. `list.getY()`.
    pub(super) content_top: f32,
    /// The canvas this was arranged at, so band offsets can be made relative to
    /// it.
    canvas: (f32, f32),
}

impl ServerListBlock {
    /// Arrange the tree at `width`×`height` and read its leaves back. The leaf
    /// counts are asserted for [`MenuBlock::of`]'s reason.
    pub(super) fn at(width: f32, height: f32) -> Self {
        let root = server_list_layout(width, height);
        let header = layout::widget_rects(root.header());
        let footer = layout::widget_rects(root.footer());
        assert_eq!(
            header.len(),
            1,
            "the multiplayer header has {} leaves, the screen has 1 (the title)",
            header.len()
        );
        assert_eq!(
            footer.len(),
            super::nav::SERVER_LIST_BUTTONS.len(),
            "the multiplayer footer has {} leaves, the screen has {}",
            footer.len(),
            super::nav::SERVER_LIST_BUTTONS.len()
        );
        Self {
            title: header[0],
            footer,
            content_top: root.contents().y(),
            canvas: (width, height),
        }
    }

    /// The footer leaf `index` as a slot measured from [`Origin::ScreenBottom`].
    /// Its `dy` is negative — the footer is pinned to the bottom edge.
    pub(super) fn footer_slot(&self, index: usize) -> Slot {
        let (x, y, w, h) = self.footer[index];
        Slot {
            origin: Origin::ScreenBottom,
            dx: x - self.canvas.0 * 0.5,
            dy: y - self.canvas.1,
            w,
            h,
        }
    }
}

/// The multiplayer screen, arranged once at [`SERVER_LIST_REF_CANVAS`].
///
/// Canvas-independence is the same argument, and it is asserted the same way:
/// `the_server_list_slots_do_not_depend_on_the_reference_canvas` re-arranges at
/// three sizes and requires every slot to come out identical. The footer column
/// measures 308 wide at any width (both its rows do — `3 * 100 + 2 * 4` and
/// `4 * 74 + 3 * 4`), and the content band always begins at the header height,
/// because the list is sized to `getContentHeight()` exactly.
fn server_list_block() -> &'static ServerListBlock {
    static BLOCK: std::sync::OnceLock<ServerListBlock> = std::sync::OnceLock::new();
    BLOCK.get_or_init(|| ServerListBlock::at(SERVER_LIST_REF_CANVAS.0, SERVER_LIST_REF_CANVAS.1))
}

/// The canvas [`server_list_block`] arranges at. See that function.
pub(super) const SERVER_LIST_REF_CANVAS: (f32, f32) = (854.0, 480.0);

/// The screen title, positioned from the arranged header's own title cell.
///
/// `Align::Centre` because that cell is zero-width and therefore *is* the text's
/// centre — see [`server_list_layout`].
#[must_use]
pub fn server_list_title_label() -> MenuLabel {
    let block = server_list_block();
    let (x, y, _, _) = block.title;
    MenuLabel {
        text: super::nav::SERVER_LIST_TITLE.to_string(),
        origin: Origin::ScreenTop,
        dx: x - block.canvas.0 * 0.5,
        dy: y,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

/// Vanilla's rect for one footer button, read out of the arranged footer.
///
/// Exhaustive rather than an `as usize`, for [`title_slot`]'s reason: a new
/// variant must be a compile error, not a silent off-by-one across every rect.
#[must_use]
pub fn server_list_footer_slot(button: super::nav::ServerListButton) -> Slot {
    use super::nav::ServerListButton as B;
    let index = match button {
        B::Select => 0,
        B::Direct => 1,
        B::Add => 2,
        B::Edit => 3,
        B::Delete => 4,
        B::Refresh => 5,
        B::Back => 6,
    };
    server_list_block().footer_slot(index)
}

/// The left edge of every list row: `getRowLeft()`, which is
/// `getX() + this.width / 2 - getRowWidth() / 2` with `getX() == 0`
/// (`AbstractSelectionList.java`).
///
/// **Not `(width - 305) / 2`.** Vanilla halves each term separately with integer
/// division, so at an odd width the two differ by a pixel; the `floor`s are that
/// arithmetic, and they are why this takes a width instead of folding into a
/// [`Slot`]'s `dx`.
#[must_use]
pub fn server_row_left(width: f32) -> f32 {
    (width * 0.5).floor() - (SERVER_LIST_ROW_W * 0.5).floor()
}

/// The top of list row `index`: `getFirstEntryY() + index * itemHeight -
/// scrollAmount` — `repositionEntries` (`AbstractSelectionList.java`).
///
/// **`scroll` is pixels, not rows.** It was a row count when this
/// landed for that fix, for a reason that was true then and is not now: the pipeline
/// had no scissor, so a straddling row would have painted over the header or the
/// footer instead of being cut. `draw_server_entry` is wrapped in
/// [`Quads::with_clip`] against the list's band now, so a partial row is clipped
/// exactly as vanilla's `enableScissor` clips it — which is what lets this take
/// the continuous offset vanilla's `scrollAmount` has always been. The player-
/// visible consequence of the old model was one wheel notch jumping a whole 36 px
/// entry; see `super::nav::MenuNav::server_scroll`.
#[must_use]
pub fn server_row_top(index: usize, scroll: f32) -> f32 {
    server_list_block().content_top + SERVER_LIST_FIRST_ENTRY_Y + index as f32 * SERVER_LIST_ITEM_H
        - scroll
}

/// The rect of list row `index` at a `width`-wide canvas, scrolled by `scroll`
/// **pixels**.
#[must_use]
pub fn server_row_rect(index: usize, width: f32, scroll: f32) -> (f32, f32, f32, f32) {
    (
        server_row_left(width),
        server_row_top(index, scroll),
        SERVER_LIST_ROW_W,
        SERVER_LIST_ITEM_H,
    )
}

/// A row's *content* rect — the entry rect inset by
/// [`SERVER_LIST_ENTRY_PADDING`] on each side
/// (`AbstractSelectionList.java`). Everything an
/// `OnlineServerEntry` draws is measured from this, not from the row.
#[must_use]
pub fn server_row_content_rect(index: usize, width: f32, scroll: f32) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = server_row_rect(index, width, scroll);
    (
        x + SERVER_LIST_ENTRY_PADDING,
        y + SERVER_LIST_ENTRY_PADDING,
        w - 2.0 * SERVER_LIST_ENTRY_PADDING,
        h - 2.0 * SERVER_LIST_ENTRY_PADDING,
    )
}

/// Whether row `index` is inside the list's band on a `height`-tall canvas at
/// `scroll` **pixels** of offset — `extractListItems`' own visibility test,
/// `child.getY() + child.getHeight() >= getY() && child.getY() <= getBottom()`
/// (`AbstractSelectionList.java`).
///
/// `row_rect` calls this too (through [`MenuRow::entry`]'s carried `scroll`), so
/// a click can no longer land on a row that is not on screen — see
/// `docs/server-list.md`'s `hit_testing_matches_what_is_drawn_after_scrolling`
/// for the executed control.
///
/// **The `index < scroll` early reject is gone, and its removal is
/// the point rather than a tidy-up.** With a row-quantized offset this function
/// stood in for a scissor the pipeline did not have, so it rejected a row
/// *partly* above the band outright rather than let it be half-drawn over the
/// header. `draw_server_entry` runs inside [`Quads::with_clip`] now, so a
/// straddling row is cut exactly as vanilla cuts it — and with a pixel offset a
/// straddling row is the *normal* case, not an edge one: at `scroll = 18.0`, row
/// 0 is half above the band and must still draw its visible half. Keeping the
/// reject would have made every intermediate scroll position drop a row, which
/// is a worse artefact than the 36 px stepping this replaced. The inclusive band
/// test below is now the only gate, and it is vanilla's.
#[must_use]
pub fn server_row_visible(index: usize, height: f32, scroll: f32) -> bool {
    let top = server_row_top(index, scroll);
    let list_top = server_list_block().content_top;
    let list_bottom = height - SERVER_LIST_FOOTER_H;
    top + SERVER_LIST_ITEM_H >= list_top && top <= list_bottom
}

/// Rows guaranteed visible at [`crate::config::MIN_SCALED_HEIGHT`] (vanilla's
/// `Window.java`), so scroll-into-view (keyboard) and the wheel's fallback
/// clamp are correct at every canvas and merely conservative at a larger one —
/// the same trade `options::LIST_WINDOW_PX` and `accounts::VISIBLE_ROWS` make,
/// for the same reason named on [`server_row_visible`]: this pipeline has no
/// scissor, so a window that ever *overestimates* what fits would paint a row
/// over the footer. Row-quantized rather than `LIST_WINDOW_PX`'s pixels, for
/// [`server_row_top`]'s reason.
#[must_use]
pub fn server_list_window_rows() -> usize {
    let list_top = server_list_block().content_top;
    let band = crate::config::MIN_SCALED_HEIGHT as f32 - list_top - SERVER_LIST_FOOTER_H;
    (band / SERVER_LIST_ITEM_H).floor().max(1.0) as usize
}

/// The multiplayer list's scroll model — a [`widget::ScrollList`] over
/// `entry_count` rows in the band this screen's rows are actually placed in, or
/// `None` when there is no band (an empty list, or a canvas too short to have
/// one).
///
/// **This is the single expression the whole screen's scrolling derives from**
/// and that is its entire reason to exist. `MenuNav`'s wheel
/// handler drives `mouse_scrolled`/`set_scroll` through it, so the 18 px notch
/// rate and `setScrollAmount`'s clamp come from the primitive rather than being
/// restated; [`server_scroll_list`] rebuilds it per frame for the scrollbar; and
/// [`server_row_top`] places the rows from the offset it produced. A thumb
/// computed from a separate expression is how a bar and its rows desynchronise,
/// which is the failure the pixel-offset conversion had to avoid.
///
/// Band and pitch are derived, never restated: `server_list_block().content_top`,
/// [`SERVER_LIST_FOOTER_H`] and [`SERVER_LIST_ITEM_H`] are the same three values
/// the draw uses.
#[must_use]
pub fn server_scroll_model(entry_count: usize, height: f32) -> Option<widget::ScrollList> {
    server_list_spec(entry_count, 0.0).model(height)
}

/// This screen's list as the generic [`widget::ListSpec`], which is what
/// `MenuNav::active_list` hands the scrollbar draw and the wheel arm.
///
/// [`server_scroll_model`] is now a thin wrapper over this rather than the other way
/// round, so the band and pitch are stated **once** and the generic hook cannot
/// disagree with the screen-specific helpers (`server_row_top`,
/// `server_list_max_scroll`) about where the list is. That direction matters: a spec
/// that rebuilt the band itself would be a second expression, which is the exact
/// failure `server_scroll_model`'s own doc exists to prevent.
#[must_use]
pub fn server_list_spec(entry_count: usize, scroll: f32) -> widget::ListSpec {
    widget::ListSpec::uniform(
        SERVER_LIST_ITEM_H,
        server_list_block().content_top,
        SERVER_LIST_FOOTER_H,
        entry_count,
        SERVER_LIST_ROW_W,
    )
    .at(scroll)
}

/// The largest legal `scroll` for `entry_count` rows at a `height`-tall canvas,
/// **in pixels** — vanilla's `AbstractScrollArea::maxScrollAmount`,
/// `max(0, contentHeight - height)`.
///
/// Delegates to [`server_scroll_model`] rather than recomputing the band, so the
/// clamp the wheel applies and the extent the scrollbar draws cannot disagree.
/// `0.0` when the list does not scroll at all.
#[must_use]
pub fn server_list_max_scroll(entry_count: usize, height: f32) -> f32 {
    server_scroll_model(entry_count, height).map_or(0.0, |l| l.max_scroll())
}

/// The favicon's rect in row `index` — the content origin, 32×32
/// (`ServerSelectionList.java`).
///
/// **Public because the click needs it too.** `MenuNav::click` decides whether a
/// click joins, moves the row up or moves it down from which quadrant of *this*
/// rect the cursor is in, and a second copy of the arithmetic is how the
/// highlighted quadrant and the acting quadrant drift apart.
#[must_use]
pub fn server_entry_icon_rect(index: usize, width: f32, scroll: f32) -> (f32, f32, f32, f32) {
    let (cx, cy, _, _) = server_row_content_rect(index, width, scroll);
    (cx, cy, SERVER_ENTRY_ICON, SERVER_ENTRY_ICON)
}

/// The rect of the status icon in row `index`, and the x the status *text* is
/// right-aligned against.
///
/// `statusIconX = getContentRight() - 10 - 5` (`ServerSelectionList.java`),
/// at `getContentY()` — the icon is **not** vertically centred in the row.
#[must_use]
pub fn server_status_icon_rect(index: usize, width: f32, scroll: f32) -> (f32, f32, f32, f32) {
    let (cx, cy, cw, _) = server_row_content_rect(index, width, scroll);
    (
        cx + cw - SERVER_STATUS_ICON_W - SERVER_ENTRY_SPACING,
        cy,
        SERVER_STATUS_ICON_W,
        SERVER_STATUS_ICON_H,
    )
}

