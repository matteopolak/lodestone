//! Vanilla's `SelectWorldScreen` (issue #397): its metrics, its
//! header/footer layout block, and the world-list row rects.
//!
//! Named `world_list` rather than `world_select` on purpose: this module is a
//! child of `render`, so a sibling's `super::world_select::…` — which means
//! `crate::menu::world_select`, a different module — would resolve here
//! instead.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

// -- vanilla's `SelectWorldScreen` metrics (issue #397) ----------------------
//
// Same rule as the block above: every number is transcribed from
// `.cache/mc/26.2/client-src`, with the file and line named, in logical GUI
// pixels.

/// The header band, spelled out as `8 + 9 + 8 + 20 + 4` in the constructor
/// (`SelectWorldScreen.java:31`) and left unreduced here for the reason it is
/// unreduced there: the parts *are* the layout — 8 px of slack above and below,
/// the 9 px title `StringWidget`, the 20 px search box, and the 4 px
/// `LinearLayout` spacing between the two.
const WORLD_SELECT_HEADER_H: f32 = 8.0 + 9.0 + 8.0 + 20.0 + 4.0;
/// The footer band (`SelectWorldScreen.java:31`). Two 20 px button rows 4 px
/// apart measure 44, so the band carries 16 px of slack, which the footer
/// `FrameLayout`'s inherited `align(0.5, 0.5)` splits 8/8.
const WORLD_SELECT_FOOTER_H: f32 = 60.0;
/// `LinearLayout.vertical().spacing(4)` in the header and `.rowSpacing(4)` in
/// the footer grid (`SelectWorldScreen.java:46,82`) — the same 4 either way.
const WORLD_SELECT_SPACING: i32 = 4;
/// `new GridLayout().columnSpacing(8)` (`SelectWorldScreen.java:82`).
const WORLD_SELECT_COLUMN_SPACING: i32 = 8;
/// `footer.createRowHelper(4)` (`SelectWorldScreen.java:84`).
const WORLD_SELECT_FOOTER_COLUMNS: usize = 4;
/// The search box's declared size — `new EditBox(font, this.width / 2 - 100, 22,
/// 200, 20, …)` (`SelectWorldScreen.java:55`). **The `x` and `y` in that call
/// are dead**: the header `LinearLayout` overwrites both when it arranges, which
/// is why the box lands at y `21` rather than the 22 written there.
const WORLD_SELECT_SEARCH_W: f32 = 200.0;
/// `.width(71)` on Edit / Delete / Re-Create / Back
/// (`SelectWorldScreen.java:91,96,103,106`). Play and Create take
/// `Button.DEFAULT_WIDTH` instead ([`widget::DEFAULT_WIDTH`]) and each spans two
/// of the four columns.
const WORLD_SELECT_SMALL_BTN_W: f32 = 71.0;
/// A `StringWidget`'s height: `StringWidget(message, font)` delegates to
/// `this(0, 0, font.width(...), 9, ...)` (`StringWidget.java:18-20`).
const STRING_WIDGET_H: f32 = 9.0;
/// `WorldSelectionList.getRowWidth()` (`WorldSelectionList.java:247-249`) — a
/// 270 px override of `AbstractSelectionList`'s own 220 (`:389-391`).
const WORLD_LIST_ROW_W: f32 = 270.0;
/// The list's `itemHeight`: the last argument of
/// `super(minecraft, width, height, 0, 36)` (`WorldSelectionList.java:112`).
const WORLD_LIST_ITEM_H: f32 = 36.0;
/// `AbstractSelectionList.Entry.CONTENT_PADDING` (`:436`). Every `getContentX`/
/// `getContentY` insets the entry rect by 2 and `getContentWidth`/
/// `getContentHeight` by 4 (`:477-495`), so a 36 px row has a **32** px content
/// box — which is exactly the world icon's 32×32 (`WorldListEntry.ICON_SIZE`,
/// `:400`).
const LIST_CONTENT_PADDING: f32 = 2.0;
/// `getFirstEntryY() = getY() + 2` (`AbstractSelectionList.java:104-106`): the
/// gap above the first row. Not [`LIST_CONTENT_PADDING`], even though it is also
/// 2 — they are different expressions and only one of them scales with a row.
const WORLD_LIST_FIRST_ENTRY_Y: f32 = 2.0;

/// The canvas [`world_select_block`] arranges its tree at.
///
/// [`layout::HeaderAndFooterLayout`] is the first container here that is
/// **canvas-dependent** — it pins the footer to `screen.height` and centres both
/// bands in `screen.width` — so unlike [`title_block`] and [`pause_block`] its
/// arranged rects are not directly reusable at another size. What *is*
/// canvas-independent is every rect once it is expressed relative to the right
/// [`Origin`]: the header column measures 200 wide and the footer grid 308
/// whatever the screen is, and the content band always begins at the header
/// height (see [`WorldSelectBlock::at`]). The slots are therefore derived once
/// here and asserted invariant at three different canvases by
/// `the_world_select_slots_do_not_depend_on_the_reference_canvas` — which is the
/// only thing standing between this and a screen that is correct at 854×480 and
/// wrong everywhere else.
const WORLD_SELECT_REF_CANVAS: (f32, f32) = (854.0, 480.0);

/// Vanilla's `SelectWorldScreen.init` (`SelectWorldScreen.java:44-107`) as a
/// real [`layout::HeaderAndFooterLayout`], arranged for a `width`×`height`
/// canvas.
///
/// Three things about it are worth knowing before changing it:
///
/// - **The title cell is zero-width on purpose.** Vanilla's
///   `StringWidget(this.title, this.font)` is `font.width(title)` wide, and this
///   shell has no font at arrange time. It does not matter: the cell is
///   `alignHorizontallyCenter`ed in the 200 px column, so a `w`-wide title lands
///   at `colX + (200 - w) / 2` and its *centre* is `colX + 100` for every `w`
///   short of 200. A zero-width cell puts the leaf rect exactly on that centre,
///   which is what [`world_select_title_label`] draws from — so the arranged
///   position is the real one rather than an approximation of it.
/// - **The list is a [`layout::SpacerElement`], not a widget.** It has to take
///   part in the measurement, because `HeaderAndFooterLayout`'s content clamp
///   reads the content frame's *height* (`min(headerHeight + 30, screenHeight -
///   footerHeight - contentHeight)`), and vanilla sizes the list to
///   `layout.getContentHeight()` exactly (`:68`) — which is what makes the clamp
///   pick the header height. A spacer's `visit_widgets` is a no-op, so it is
///   measured and never drawn, which is also true of vanilla's list here: it is
///   an `AbstractWidget` but not one this shell has ported.
/// - **`SharedConstants.DEBUG_WORLD_RECREATE` is a system-property debug flag**
///   (`SharedConstants.java:119`), false in any shipped client, so the sub-header
///   holds the search box alone (`:50-53`).
fn world_select_layout(width: f32, height: f32) -> layout::HeaderAndFooterLayout {
    let cell = |w: f32, h: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, h, ""))
    };
    let mut root = layout::HeaderAndFooterLayout::with_heights(
        width,
        height,
        WORLD_SELECT_HEADER_H,
        WORLD_SELECT_FOOTER_H,
    );

    // `LinearLayout header = this.layout.addToHeader(LinearLayout.vertical().spacing(4));`
    // `header.defaultCellSetting().alignHorizontallyCenter();` (`:46-47`)
    let mut header = layout::LinearLayout::vertical().spacing(WORLD_SELECT_SPACING);
    {
        let baseline = header.default_cell_setting();
        *baseline = baseline.align_horizontally_center();
    }
    header.add_child(Box::new(Widget::new(
        0.0,
        0.0,
        0.0,
        STRING_WIDGET_H,
        "Select World",
    )));
    let mut sub_header = layout::LinearLayout::horizontal().spacing(WORLD_SELECT_SPACING);
    sub_header.add_child(cell(WORLD_SELECT_SEARCH_W, WIDGET_H));
    header.add_child(Box::new(sub_header));
    root.add_to_header(Box::new(header));

    // The list, sized to the content band exactly as `:67-68` does.
    let content_height = root.content_height();
    root.add_to_contents(Box::new(layout::SpacerElement::new(width, content_height)));

    // `GridLayout footer = this.layout.addToFooter(new GridLayout().columnSpacing(8).rowSpacing(4));`
    // `footer.defaultCellSetting().alignHorizontallyCenter();` (`:82-84`)
    let mut footer = layout::GridLayout::new()
        .column_spacing(WORLD_SELECT_COLUMN_SPACING)
        .row_spacing(WORLD_SELECT_SPACING);
    {
        let baseline = footer.default_cell_setting();
        *baseline = baseline.align_horizontally_center();
    }
    let mut helper = footer.create_row_helper(WORLD_SELECT_FOOTER_COLUMNS);
    // Row 1: Play and Create, `Button.DEFAULT_WIDTH` each, two columns each
    // (`:85-88`). Their 150 px is what sets all four column widths: a two-column
    // span of 150 with an 8 px gutter splits 71/71 through `Divisor`, and the
    // four 71 px buttons below can then only *match* it.
    for _ in 0..2 {
        helper.add_spanning(cell(widget::DEFAULT_WIDTH, WIDGET_H), 2);
    }
    // Row 2: Edit, Delete, Re-Create, Back — one column each (`:89-106`).
    for _ in 0..4 {
        helper.add_child(cell(WORLD_SELECT_SMALL_BTN_W, WIDGET_H));
    }
    drop(helper);
    root.add_to_footer(Box::new(footer));

    root.arrange_elements();
    root
}

/// One arranged world-select screen: the header's leaf rects, the footer's, and
/// where the content band starts.
///
/// Split by band rather than flattened into one `Vec` the way [`MenuBlock`] is,
/// because the two bands are anchored to *different* [`Origin`]s — the header to
/// the top of the screen and the footer to the bottom — so a flat list of
/// absolute rects could not be converted to canvas-independent offsets.
#[derive(Debug)]
pub(super) struct WorldSelectBlock {
    /// The header column's leaves, in insertion order: the title cell, then the
    /// search box.
    header: Vec<(f32, f32, f32, f32)>,
    /// The footer grid's leaves, in [`super::world_select::WORLD_SELECT_BUTTONS`]'
    /// order.
    footer: Vec<(f32, f32, f32, f32)>,
    /// The content frame's top, i.e. `list.getY()`.
    pub(super) content_top: f32,
    /// The canvas this was arranged at, so the band offsets can be made relative
    /// to it.
    canvas: (f32, f32),
}

impl WorldSelectBlock {
    /// Arrange the tree at `width`×`height` and read its leaves back.
    ///
    /// The leaf counts are asserted rather than trusted, for [`MenuBlock::of`]'s
    /// reason: a tree that no longer describes the screen must fail loudly
    /// instead of silently shifting every rect by one.
    pub(super) fn at(width: f32, height: f32) -> Self {
        let root = world_select_layout(width, height);
        let header = layout::widget_rects(root.header());
        let footer = layout::widget_rects(root.footer());
        assert_eq!(
            header.len(),
            2,
            "the world-select header has {} leaves, the screen has 2 (title, search)",
            header.len()
        );
        assert_eq!(
            footer.len(),
            super::world_select::WORLD_SELECT_BUTTONS.len(),
            "the world-select footer has {} leaves, the screen has {}",
            footer.len(),
            super::world_select::WORLD_SELECT_BUTTONS.len()
        );
        Self {
            header,
            footer,
            content_top: root.contents().y(),
            canvas: (width, height),
        }
    }

    /// A header leaf as a slot measured from [`Origin::ScreenTop`].
    pub(super) fn header_slot(&self, index: usize) -> Slot {
        let (x, y, w, h) = self.header[index];
        Slot {
            origin: Origin::ScreenTop,
            dx: x - self.canvas.0 * 0.5,
            dy: y,
            w,
            h,
        }
    }

    /// A footer leaf as a slot measured from [`Origin::ScreenBottom`]. Its `dy`
    /// is negative — the footer is pinned to the bottom edge.
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

/// The world-select screen, arranged once at [`WORLD_SELECT_REF_CANVAS`]. See
/// [`title_block`] on why arranging once is safe, and
/// [`WORLD_SELECT_REF_CANVAS`] on the extra condition that applies here.
pub(super) fn world_select_block() -> &'static WorldSelectBlock {
    static BLOCK: std::sync::OnceLock<WorldSelectBlock> = std::sync::OnceLock::new();
    BLOCK.get_or_init(|| {
        WorldSelectBlock::at(WORLD_SELECT_REF_CANVAS.0, WORLD_SELECT_REF_CANVAS.1)
    })
}

/// The search box's rect, read out of the arranged header.
#[must_use]
pub fn world_select_search_slot() -> Slot {
    world_select_block().header_slot(1)
}

/// Vanilla's rect for one footer button, read out of the arranged grid.
///
/// Exhaustive rather than an `as usize`, for [`title_slot`]'s reason: a new
/// variant must be a compile error, not a silent off-by-one across every rect.
#[must_use]
pub fn world_select_slot(button: super::world_select::WorldSelectButton) -> Slot {
    use super::world_select::WorldSelectButton as B;
    let index = match button {
        B::Play => 0,
        B::Create => 1,
        B::Edit => 2,
        B::Delete => 3,
        B::ReCreate => 4,
        B::Back => 5,
    };
    world_select_block().footer_slot(index)
}

/// The title `StringWidget`'s label, positioned from the arranged header's own
/// title cell.
///
/// `Align::Centre` because the cell is zero-width and therefore *is* the text's
/// centre — see [`world_select_layout`]. `StringWidget.visitLines` draws at
/// `y + (height - 9) / 2`, which is `y` for a 9 px widget
/// (`StringWidget.java:64`), so the cell's `y` is the text's top.
#[must_use]
pub fn world_select_title_label() -> MenuLabel {
    let slot = world_select_block().header_slot(0);
    MenuLabel {
        text: super::world_select::WORLD_SELECT_TITLE.to_string(),
        origin: slot.origin,
        dx: slot.dx,
        dy: slot.dy,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

/// The top of world-list row `index`, in logical pixels.
///
/// `getFirstEntryY() + index * itemHeight` with no scroll term, because
/// `scrollAmount` is 0 for a list that cannot overflow its band — which is the
/// only list this screen has (see [`super::world_select`]). Canvas-independent:
/// `list.getY()` is the content band's top, which
/// `HeaderAndFooterLayout.arrangeElements` clamps to exactly the header height
/// whenever the content is sized to `getContentHeight()`.
#[must_use]
pub fn world_list_row_top(index: usize) -> f32 {
    world_select_block().content_top + WORLD_LIST_FIRST_ENTRY_Y + index as f32 * WORLD_LIST_ITEM_H
}

/// The left edge of every world-list row: `getRowLeft()`, which is
/// `getX() + this.width / 2 - getRowWidth() / 2` with `getX() == 0`
/// (`AbstractSelectionList.java:372-374`). The `floor` is Java's integer
/// division of an odd canvas width, and it is the reason this takes a width
/// rather than being folded into a slot.
#[must_use]
pub fn world_list_row_left(width: f32) -> f32 {
    (width * 0.5).floor() - (WORLD_LIST_ROW_W * 0.5).floor()
}

/// The rect of world-list row `index` at a `width`-wide canvas.
#[must_use]
pub fn world_list_row_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    (
        world_list_row_left(width),
        world_list_row_top(index),
        WORLD_LIST_ROW_W,
        WORLD_LIST_ITEM_H,
    )
}

/// A row's *content* rect — the entry rect inset by
/// [`LIST_CONTENT_PADDING`]/twice it (`AbstractSelectionList.java:477-495`).
/// This is where a `WorldListEntry` puts its 32×32 icon and, at
/// `x + 32 + 3`, its three text lines (`WorldSelectionList.java:494-502,569-571`).
#[must_use]
pub fn world_list_row_content_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = world_list_row_rect(index, width);
    (
        x + LIST_CONTENT_PADDING,
        y + LIST_CONTENT_PADDING,
        w - 2.0 * LIST_CONTENT_PADDING,
        h - 2.0 * LIST_CONTENT_PADDING,
    )
}

/// The one entry the list has, drawn with vanilla's `NoWorldsEntry` geometry —
/// `text` is [`super::world_select::WorldSelectNav::world_row_label`], i.e. the
/// bundled world (issue #287).
///
/// `NoWorldsEntry`'s geometry rather than `WorldListEntry`'s is deliberate and
/// unchanged by #287: `WorldListEntry` draws a 32×32 icon plus three text lines
/// off a `LevelSummary` (`WorldSelectionList.java:494-502`), and there is no
/// world storage here to supply one. See `world_select`'s module docs.
///
/// `NoWorldsEntry.extractContent` centres a `StringWidget` in the entry's
/// content box — `setPosition(getContentXMiddle() - width / 2, getContentYMiddle()
/// - height / 2)` (`WorldSelectionList.java:392-396`) — and a `StringWidget`'s
/// own draw then adds `(height - 9) / 2`, which is 0 for its 9 px height. So the
/// text's top is `contentYMiddle - 4`, and the `4` is Java's `9 / 2`, not 4.5.
///
/// `contentXMiddle` is `getContentX() + getContentWidth() / 2` =
/// `rowLeft + 2 + 133`, and `rowLeft` is `floor(w / 2) - 135`, so the two 133/135
/// halves cancel and the centre is `floor(w / 2)` — the screen's own centre,
/// which is why this is an [`Origin::ScreenTop`] label with `dx: 0`.
#[must_use]
pub fn world_list_row_label(text: &str) -> MenuLabel {
    // The `x` and `w` of the content rect are discarded — the label is centred on
    // the screen for the reason above — so the width passed here is arbitrary.
    // Reading the rect anyway, instead of restating `row_top + 2`, is
    // `CLAUDE.md`'s "derive layout from the same expression the draw uses".
    let (_, content_y, _, content_h) = world_list_row_content_rect(0, 0.0);
    MenuLabel {
        text: text.to_string(),
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: content_y + (content_h * 0.5).floor() - (STRING_WIDGET_H * 0.5).floor(),
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

