//! The account screen: its metrics (referred to this repo's own
//! `JoinMultiplayerScreen` port, because vanilla has no such screen), its
//! layout block, row rects, and the three frames it can be in.
//!
//! Named `account_screen` rather than `accounts` for [`super::world_list`]'s
//! reason: `super::accounts` inside this subtree has to keep meaning
//! `crate::menu::accounts`.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;
use super::measure::clip;

// -- the account screen's metrics ---------------------------------------------
//
// **There is no accounts screen in vanilla to port.** Minecraft picks an account
// in the launcher, outside the game — see `nav::MainButton::Accounts`, which
// says the same thing about the title-screen button that opens this. So the
// reference for every number below is *this repo's own* `JoinMultiplayerScreen`
// port two screens up: a `HeaderAndFooterLayout` title, a footer of
// `LinearLayout`-arranged buttons, and 36 px list rows in a 305 px column. Each
// constant therefore cites the server-list constant it deliberately matches
// rather than a jar line, and the two that differ say why they differ.
//
// Separate constants rather than reusing the `SERVER_LIST_*` ones, for the
// reason that block states in its own header comment: the agreement is a
// *choice*, and a shared constant would make a change to the multiplayer screen
// silently move this one.

/// The header band: [`SERVER_LIST_HEADER_H`]'s 33, which is also
/// [`layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT`] — one 9 px title `StringWidget`
/// with 12 px of slack either side.
const ACCOUNTS_HEADER_H: f32 = 33.0;
/// The footer band: [`SERVER_LIST_FOOTER_H`]'s 60, **even though this screen's
/// footer is one row of buttons rather than two**. The 40 px a single 20 px row
/// leaves is not waste: the `FrameLayout` splits it 20/20, and the lower half is
/// where the key-hint line sits (see [`accounts_hint_dy`]). A 33 px band would
/// put that line off the bottom of the canvas.
pub(super) const ACCOUNTS_FOOTER_H: f32 = 60.0;
/// `LinearLayout.horizontal().spacing(4)` — [`SERVER_LIST_FOOTER_SPACING`].
pub(super) const ACCOUNTS_FOOTER_SPACING: i32 = 4;
/// One footer button: [`SERVER_LIST_LOWER_BUTTON_W`]'s 74, so the four of them
/// measure `4 * 74 + 3 * 4 = 308` — the same footer column width the
/// multiplayer screen's lower row has, which is what makes the two screens line
/// up rather than each being centred to its own width.
pub(super) const ACCOUNTS_BUTTON_W: f32 = 74.0;
/// A list row's pitch: [`SERVER_LIST_ITEM_H`]'s 36. With
/// [`ACCOUNTS_ENTRY_PADDING`] a side that leaves a **32** px content box, which
/// is exactly [`ACCOUNTS_HEAD_ICON`] — the head fills the row's height the same
/// way a favicon does.
const ACCOUNTS_ITEM_H: f32 = 36.0;
/// A list row's width: [`SERVER_LIST_ROW_W`]'s 305.
pub(super) const ACCOUNTS_ROW_W: f32 = 305.0;
/// `AbstractSelectionList.Entry.CONTENT_PADDING`'s 2, per side.
const ACCOUNTS_ENTRY_PADDING: f32 = 2.0;
/// `getFirstEntryY() = getY() + 2` — the gap above row 0. A different
/// expression from [`ACCOUNTS_ENTRY_PADDING`] that happens to be the same 2;
/// only one of them insets a row.
const ACCOUNTS_FIRST_ENTRY_Y: f32 = 2.0;
/// The head icon, [`SERVER_ENTRY_ICON`]'s 32 — the content box's full height.
pub(super) const ACCOUNTS_HEAD_ICON: f32 = 32.0;
/// The gap between the head icon and the text column, [`SERVER_ENTRY_TEXT_GAP`].
pub(super) const ACCOUNTS_TEXT_GAP: f32 = 3.0;
/// The gap the trailing "Selected" column keeps from the content's right edge,
/// and from the name — [`SERVER_ENTRY_SPACING`].
pub(super) const ACCOUNTS_SPACING: f32 = 5.0;
/// The detail line's offset below the content's top, [`SERVER_ENTRY_MOTD_Y`].
pub(super) const ACCOUNTS_DETAIL_Y: f32 = 12.0;
/// A `StringWidget`'s height — what the title header is.
const ACCOUNTS_TITLE_H: f32 = 9.0;
/// The account list's own title.
const ACCOUNTS_TITLE: &str = "Accounts";
/// The sign-in sub-flow's title.
const ACCOUNTS_SIGN_IN_TITLE: &str = "Sign in with Microsoft";
/// The failure state's title.
const ACCOUNTS_FAILED_TITLE: &str = "Sign-in failed";
/// The offline-name editor's title.
const ACCOUNTS_EDIT_NAME_TITLE: &str = "Edit offline name";
/// How many lines a save-error notice is allowed. Two, because it sits *above*
/// the footer band and therefore grows upward into the list — unlike the
/// sign-in states' notice, which owns the whole content band.
const ACCOUNTS_SAVE_ERROR_LINES: f32 = 2.0;

/// A row's detail line: `-8355712`, the same mid grey a multiplayer row's MOTD
/// uses. Its own constant for the reason above.
pub(super) const ACCOUNTS_DIM: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// The highlighted row's interior, `-16777216` — opaque black, filled inside the
/// 1 px outline, exactly `AbstractSelectionList.extractItem`'s selection pass.
pub(super) const ACCOUNTS_SELECTION_FILL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The canvas [`accounts_block`] arranges at, for [`SERVER_LIST_REF_CANVAS`]'s
/// reason and with the same condition attached: every rect must be expressible
/// relative to an [`Origin`], which
/// `the_accounts_slots_do_not_depend_on_the_reference_canvas` asserts by
/// re-arranging at three canvases.
const ACCOUNTS_REF_CANVAS: (f32, f32) = (854.0, 480.0);

/// The account screen as a real [`layout::HeaderAndFooterLayout`], arranged for
/// a `width`×`height` canvas — [`server_list_layout`]'s shape, with a
/// four-button single-row footer instead of a 3 + 4 two-row one.
///
/// The two notes from that function apply here unchanged:
///
/// - **The title cell is zero-width.** There is no font at arrange time, and the
///   header frame centres its child, so a zero-width cell lands exactly on the
///   centre a real-width one would be centred about — which is what
///   [`accounts_title_label`] draws from.
/// - **The list is a [`layout::SpacerElement`]** sized to `content_height()`, so
///   it takes part in the measurement (`HeaderAndFooterLayout`'s content clamp
///   reads the content frame's height) and is never drawn — the rows draw
///   through [`draw_account_entry`], not as widgets.
fn accounts_layout(width: f32, height: f32) -> layout::HeaderAndFooterLayout {
    let mut root = layout::HeaderAndFooterLayout::with_heights(
        width,
        height,
        ACCOUNTS_HEADER_H,
        ACCOUNTS_FOOTER_H,
    );

    root.add_to_header(Box::new(Widget::new(
        0.0,
        0.0,
        0.0,
        ACCOUNTS_TITLE_H,
        ACCOUNTS_TITLE,
    )));

    let content_height = root.content_height();
    root.add_to_contents(Box::new(layout::SpacerElement::new(width, content_height)));

    // One row, so no `alignHorizontallyCenter` baseline is needed: the footer
    // `FrameLayout` centres its single child on its own. The server list needs
    // that baseline because its two rows are different widths.
    let mut footer = layout::LinearLayout::horizontal().spacing(ACCOUNTS_FOOTER_SPACING);
    for _ in 0..super::accounts::BUTTON_COUNT {
        footer.add_child(Box::new(Widget::button(
            0.0,
            0.0,
            ACCOUNTS_BUTTON_W,
            WIDGET_H,
            "",
        )));
    }
    root.add_to_footer(Box::new(footer));

    root.arrange_elements();
    root
}

/// One arranged account screen: the title cell, the four footer buttons, and
/// where the content band starts. [`ServerListBlock`]'s shape and its reason —
/// the two bands are anchored to different [`Origin`]s.
#[derive(Debug)]
pub(super) struct AccountsBlock {
    /// The header's one leaf — the title cell.
    title: (f32, f32, f32, f32),
    /// The footer's leaves, in [`super::accounts::BUTTON_ADD`] order.
    pub(super) footer: Vec<(f32, f32, f32, f32)>,
    /// The content frame's top, i.e. the list's `getY()`.
    pub(super) content_top: f32,
    /// The canvas this was arranged at, so band offsets can be made relative
    /// to it.
    canvas: (f32, f32),
}

impl AccountsBlock {
    /// Arrange the tree at `width`×`height` and read its leaves back. The leaf
    /// counts are asserted for [`MenuBlock::of`]'s reason: a tree that no longer
    /// describes the screen must fail loudly rather than shift every rect by one.
    pub(super) fn at(width: f32, height: f32) -> Self {
        let root = accounts_layout(width, height);
        let header = layout::widget_rects(root.header());
        let footer = layout::widget_rects(root.footer());
        assert_eq!(
            header.len(),
            1,
            "the account header has {} leaves, the screen has 1 (the title)",
            header.len()
        );
        assert_eq!(
            footer.len(),
            super::accounts::BUTTON_COUNT,
            "the account footer has {} leaves, the screen has {}",
            footer.len(),
            super::accounts::BUTTON_COUNT
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

/// The account screen, arranged once at [`ACCOUNTS_REF_CANVAS`].
pub(super) fn accounts_block() -> &'static AccountsBlock {
    static BLOCK: std::sync::OnceLock<AccountsBlock> = std::sync::OnceLock::new();
    BLOCK.get_or_init(|| AccountsBlock::at(ACCOUNTS_REF_CANVAS.0, ACCOUNTS_REF_CANVAS.1))
}

/// The rect for account-screen button `index` (see
/// [`super::accounts::BUTTON_ADD`] and its siblings), read out of the arranged
/// footer rather than computed here — so the width that reaches pixels is the
/// one the layout produced.
pub(super) fn accounts_button_slot(index: usize) -> Slot {
    accounts_block().footer_slot(index.min(super::accounts::BUTTON_COUNT - 1))
}

/// The single wide button the sign-in and failure states show, centred at the
/// same y the four action buttons occupy.
///
/// The y and height come off [`accounts_button_slot`] rather than being restated,
/// so the one-button states and the four-button state cannot end up on different
/// lines.
fn accounts_wide_button_slot() -> Slot {
    let row = accounts_button_slot(0);
    Slot {
        origin: row.origin,
        dx: -(widget::DEFAULT_WIDTH * 0.5),
        dy: row.dy,
        w: widget::DEFAULT_WIDTH,
        h: row.h,
    }
}

/// The key-hint line's offset from the bottom edge: 8 px below the arranged
/// button row, in the lower half of the slack [`ACCOUNTS_FOOTER_H`] leaves.
///
/// Derived from the arranged row rather than written as a constant, because the
/// failure mode of a constant here is a hint line drawn *through* the buttons —
/// and per `CLAUDE.md` a rect a gate restates is a rect that has been wrong
/// twice.
fn accounts_hint_dy() -> f32 {
    let row = accounts_button_slot(0);
    row.dy + row.h + 8.0
}

/// The screen title, positioned from the arranged header's own title cell.
///
/// `Align::Centre` because that cell is zero-width and therefore *is* the text's
/// centre — see [`accounts_layout`]. Takes the text because the three states of
/// this screen have three different titles.
fn accounts_title_label(text: &str) -> MenuLabel {
    let block = accounts_block();
    let (x, y, _, _) = block.title;
    MenuLabel {
        text: text.to_string(),
        origin: Origin::ScreenTop,
        dx: x - block.canvas.0 * 0.5,
        dy: y,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

/// The left edge of every account row: `getRowLeft()`, i.e.
/// `floor(width / 2) - floor(rowWidth / 2)`.
///
/// **Not `(width - 305) / 2`** — two separate integer divisions, which is what
/// [`server_row_left`] documents at length. Reproduced here rather than being
/// approximated by a [`Slot`]'s `dx` so that this screen's rows sit on exactly
/// the same column as the multiplayer screen's at every canvas width, odd ones
/// included.
#[must_use]
pub fn accounts_row_left(width: f32) -> f32 {
    (width * 0.5).floor() - (ACCOUNTS_ROW_W * 0.5).floor()
}

/// The y the content band starts at — the top of row 0 with nothing scrolled.
///
/// Kept as its own expression because the sign-in, failure and empty-list states
/// anchor their text to it and have **no list to scroll**: calling
/// `accounts_row_top(0, ..)` there would make a notice's position depend on an
/// offset belonging to a frame that is not being drawn.
#[must_use]
pub fn accounts_band_top() -> f32 {
    accounts_block().content_top + ACCOUNTS_FIRST_ENTRY_Y
}

/// This screen's list, as the generic [`widget::ListSpec`] the scrollbar draw and
/// the mouse wheel both go through.
///
/// **The single expression the whole screen's scrolling derives from**, the role
/// [`server_scroll_model`] plays for the multiplayer list: `AccountsNav`'s keyboard
/// cursor-follow and its wheel handler drive `ScrollList` through this, the
/// scrollbar is placed from it, and [`accounts_row_top`] positions rows from the
/// offset it produced. Band and pitch are derived from
/// `accounts_block().content_top`, [`ACCOUNTS_FOOTER_H`] and [`ACCOUNTS_ITEM_H`] —
/// the same three values the draw uses — rather than restated.
///
/// Note what this **replaced**: [`super::accounts::VISIBLE_ROWS`]'s hardcoded 5.
/// At [`crate::config::MIN_SCALED_HEIGHT`] the derived band is
/// `240 - 60 - 33 = 147` px, which `ScrollList::visible_range` resolves to exactly
/// five 36 px rows — so the constant was right, and is now *measured* instead of
/// asserted. A taller canvas legitimately shows more.
#[must_use]
pub fn accounts_list_spec(len: usize, scroll: f32) -> widget::ListSpec {
    widget::ListSpec::uniform(
        ACCOUNTS_ITEM_H,
        accounts_block().content_top,
        ACCOUNTS_FOOTER_H,
        len,
        ACCOUNTS_ROW_W,
    )
    .at(scroll)
}

/// The top of account row `index` — `getFirstEntryY() + index * itemHeight -
/// scrollAmount`, `repositionEntries` (`AbstractSelectionList.java:993-996`).
///
/// **`index` is the row's position in the full list and `scroll` is pixels.** Both
/// changed together: `index` used to be the rendered-window position because
/// [`accounts_idle_frame`] sliced the list first, which is precisely why this
/// screen could only ever sit at a whole-row offset. The `floor` is vanilla's
/// single `(int)this.scrollAmount()` truncation (`:144`) — outside the multiply, so
/// the column moves as a unit and rows stay exactly [`ACCOUNTS_ITEM_H`] apart.
#[must_use]
pub fn accounts_row_top(index: usize, scroll: f32) -> f32 {
    accounts_band_top() - scroll.floor() + index as f32 * ACCOUNTS_ITEM_H
}

/// The rect of account row `index` at a `width`-wide canvas, `scroll` px down.
#[must_use]
pub fn accounts_row_rect(index: usize, width: f32, scroll: f32) -> (f32, f32, f32, f32) {
    (
        accounts_row_left(width),
        accounts_row_top(index, scroll),
        ACCOUNTS_ROW_W,
        ACCOUNTS_ITEM_H,
    )
}

/// A row's *content* rect — the row inset by [`ACCOUNTS_ENTRY_PADDING`] a side.
/// Everything a row draws is measured from this, not from the row.
#[must_use]
pub fn accounts_row_content_rect(index: usize, width: f32, scroll: f32) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = accounts_row_rect(index, width, scroll);
    (
        x + ACCOUNTS_ENTRY_PADDING,
        y + ACCOUNTS_ENTRY_PADDING,
        w - 2.0 * ACCOUNTS_ENTRY_PADDING,
        h - 2.0 * ACCOUNTS_ENTRY_PADDING,
    )
}

/// Whether row `index` overlaps the content band at all on a `height`-tall canvas
/// — `extractListItems`' own test (`AbstractSelectionList.java:346-352`).
///
/// **This is a *partial*-overlap test now, and that is the point.** It used to be
/// one degree stricter — a row that did not fit *entirely* was skipped whole —
/// because this pipeline had no scissor and a straddling row would have painted
/// over the four footer buttons. At a pixel-granular offset a straddling row is the
/// *normal* case rather than an edge case, so skipping it would drop a row at every
/// intermediate position: a worse artefact than the 36 px stepping it replaced.
/// `draw_account_entry` is wrapped in `Quads::with_clip` against this same band, so
/// the row is **cut** instead, exactly as vanilla's `enableScissor` cuts it.
///
/// Delegates to [`widget::ScrollList::row_visible`] through
/// [`accounts_list_spec`], so the band this tests against and the band the
/// scrollbar is drawn in are one expression. `len` is unknown here, so the spec is
/// built long enough to contain `index` — visibility depends on the row's own top
/// and the band, never on how many rows follow it.
#[must_use]
pub fn accounts_row_visible(index: usize, height: f32, scroll: f32) -> bool {
    accounts_list_spec(index + 1, scroll)
        .model(height)
        .is_some_and(|l| l.row_visible(index))
}

/// The wrapped-text notice the sign-in and failure states use: the content band,
/// in the same 305 px column the rows occupy, bounded below by the footer.
///
/// This is the fix for the reported overflow. The text it carries is **not
/// ours** — [`super::accounts::describe_auth_error`] renders an `AuthError`, and
/// several of that type's variants embed a
/// snippet of whatever Microsoft or Mojang actually returned — so it must wrap
/// *and* be clipped to a rect the layout sizes, which is exactly what
/// [`MenuNotice`] is.
fn accounts_notice(text: String, colour: [f32; 4]) -> MenuNotice {
    MenuNotice {
        // Our own text, so no styled runs to preserve.
        spans: Vec::new(),
        text,
        origin: Origin::ScreenTop,
        // The row column's own left edge at an even canvas width. `dx` is
        // floored for the same reason `accounts_row_left` floors: a `Slot`-style
        // offset is `width * 0.5 + dx` unrounded, and this keeps the text block
        // on the rows' column rather than half a pixel off it.
        dx: -(ACCOUNTS_ROW_W * 0.5).floor(),
        dy: accounts_band_top(),
        w: ACCOUNTS_ROW_W,
        bottom: ACCOUNTS_FOOTER_H,
        colour,
    }
}

/// The save-error notice: the same column, but anchored **above** the footer
/// band and only [`ACCOUNTS_SAVE_ERROR_LINES`] tall, because the list owns the
/// content band on the screen this one appears on.
///
/// Placed where the multiplayer screen puts its own save-error line, and for the
/// same reason: a failed `profiles.json` write has no vanilla equivalent, and a
/// player whose account choice silently fails to persist deserves the reason.
fn accounts_save_error_notice(text: String) -> MenuNotice {
    MenuNotice {
        // Our own text, so no styled runs to preserve.
        spans: Vec::new(),
        text,
        origin: Origin::ScreenBottom,
        dx: -(ACCOUNTS_ROW_W * 0.5).floor(),
        dy: -(ACCOUNTS_FOOTER_H + LINE_H * ACCOUNTS_SAVE_ERROR_LINES + 2.0),
        w: ACCOUNTS_ROW_W,
        bottom: ACCOUNTS_FOOTER_H + 2.0,
        colour: FG_BAD,
    }
}

/// A centred line of body text in the account screen's content band, `line`
/// lines below the first row's top.
///
/// The offsets are all multiples of [`LINE_H`] from [`accounts_row_top`], so the
/// sign-in state's stack of hints sits on the same grid a list row's two text
/// lines do rather than on a second set of constants.
fn accounts_band_label(text: String, line: f32, colour: [f32; 4]) -> MenuLabel {
    MenuLabel {
        text,
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: accounts_band_top() + LINE_H * line,
        align: Align::Centre,
        colour,
        scale: 1.0,
    }
}

/// The key-hint line, centred along the bottom edge — this screen's stand-in for
/// the row-stack `footer` that a `vanilla` frame suppresses.
fn accounts_hint_label(text: &str) -> MenuLabel {
    MenuLabel {
        text: text.to_string(),
        origin: Origin::ScreenBottom,
        dx: 0.0,
        dy: accounts_hint_dy(),
        align: Align::Centre,
        colour: ACCOUNTS_DIM,
        scale: 1.0,
    }
}

/// Builds the account list's ordinary (no sign-in in flight) frame: the scrolling
/// account + offline list at [`ACCOUNTS_ITEM_H`] pitch, then the four action
/// buttons in the arranged footer.
///
/// ## Two cursors, as on the multiplayer screen
///
/// [`AccountEntryView::selected`] is the *list* cursor (`AccountsNav::highlighted`
/// — a 1 px outline over a black interior) and [`MenuFrame::selected`] is the
/// *button* the mouse is on (`AccountsNav::focus` past the end of the list, which
/// [`draw_widget`] turns into `widget/button_highlighted`). Both can be visible
/// at once, which is the whole reason they are separate fields; `usize::MAX`
/// highlights no button, which is the state whenever focus is on a row.
///
/// ## The row order is a coupling
///
/// `AccountsNav::hover` maps a **rendered** row index back through the scroll
/// window and then onto the four button slots, so the order here — `shown` list
/// rows, then Add / Select / Remove / Back — is load-bearing.
/// `the_account_rows_are_in_the_order_click_assumes` is the guard, the same shape
/// the settings and multiplayer screens carry against the same that fix bug.
#[must_use]
pub(super) fn accounts_idle_frame(accounts: &super::accounts::AccountsNav) -> MenuFrame<'static> {
    use super::accounts::{
        AccountRow, BUTTON_ADD, BUTTON_CANCEL, BUTTON_COUNT, BUTTON_REMOVE, BUTTON_SELECT,
    };

    let all_rows = accounts.rows();
    let list_len = all_rows.len();
    let scroll = accounts.scroll();
    let highlighted = accounts.highlighted();
    let focus = accounts.focus();

    // **Every** logical row, not a `rows[scroll..scroll + VISIBLE_ROWS]` slice —
    // the multiplayer list's shape. Three things follow, and they are the reason the
    // conversion is here rather than only in the offset's type:
    //
    // 1. A row's `index` is its position in the full list, so `accounts_row_top`
    //    can subtract a *pixel* offset. A sliced frame forced the index to be the
    //    rendered position, which is only expressible at whole-row offsets.
    // 2. The band is decided by `accounts_row_visible` against the **real** canvas
    //    at draw time instead of by a canvas-independent count, so a taller window
    //    legitimately shows more rows — the residual gap `VISIBLE_ROWS` documented.
    // 3. `row_rect` gates on visibility (see `measure.rs`), which is what stops a
    //    click landing on a row that scrolled out from under it.
    let mut rows: Vec<MenuRow> = all_rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let view = AccountEntryView {
                index,
                scroll,
                selected: index == highlighted,
            };
            match row {
                AccountRow::Account(p) => MenuRow {
                    label: p.username.clone(),
                    detail: "Microsoft account".to_string(),
                    trailing: if accounts.is_selected(p.profile_id) {
                        "Selected".to_string()
                    } else {
                        String::new()
                    },
                    head: Some(default_head_icon()),
                    enabled: true,
                    account: Some(view),
                    ..Default::default()
                },
                // The offline entry is not an account and has no `profile_id`;
                // `selected.is_none()` **is** its selected state — see
                // `super::accounts`' module docs before changing this.
                AccountRow::Offline => MenuRow {
                    // **The persisted name, not the string `"Play offline"`.**
                    // That literal is what made `crate::offline_identity`'s
                    // editable name unreachable: the one name every join in this
                    // client uses was stored, validated, UUID-derived — and
                    // never displayed, so nothing on screen changed when it
                    // changed. See `super::accounts`' module docs.
                    label: accounts.offline_username(),
                    detail: "No sign-in required".to_string(),
                    trailing: if accounts.offline_selected() {
                        "Selected".to_string()
                    } else {
                        String::new()
                    },
                    head: Some(default_head_icon()),
                    enabled: true,
                    account: Some(view),
                    ..Default::default()
                },
            }
        })
        .collect();

    // Explicit per-constant pushes, not a loop over a positional array: the
    // button *index* (which drives both the slot position and what
    // `AccountsNav::handle_key` does with it) must never silently drift from
    // its label just because the two lists were reordered independently.
    let button_row = |index: usize, label: &str, enabled: bool| MenuRow {
        label: label.to_string(),
        enabled,
        slot: Some(accounts_button_slot(index)),
        ..Default::default()
    };
    rows.push(button_row(BUTTON_ADD, "Add Account", true));
    rows.push(button_row(BUTTON_SELECT, "Select", true));
    // **The third slot changes identity, and is now always active.** It used to
    // read "Remove", drawn inactive whenever the cursor sat on the offline row
    // (which cannot be removed — `AccountsNav::remove_highlighted` refuses). So
    // the slot was dead space for exactly the row that needed an Edit
    // affordance, and a fifth 74 px button would overflow `MIN_SCALED_WIDTH` —
    // see `super::accounts`' module docs for that measurement. The caption comes
    // from `AccountsNav::third_button`, the same expression `activate_button`
    // dispatches on, so the label and the action cannot disagree.
    rows.push(button_row(
        BUTTON_REMOVE,
        accounts.third_button().label(),
        true,
    ));
    rows.push(button_row(BUTTON_CANCEL, "Back", true));

    // `rows` is now `list_len` list rows followed by the four buttons, so a focused
    // button's row index is `list_len + n` rather than the old `shown + n`. The two
    // agreed only while the whole list fitted in the window — with a slice, focusing
    // a button on a scrolled list pointed at the wrong row.
    let selected = if focus < list_len {
        usize::MAX
    } else {
        list_len + (focus - list_len).min(BUTTON_COUNT - 1)
    };

    let mut labels = vec![
        accounts_title_label(ACCOUNTS_TITLE),
        // **One line, whose middle term follows the third button.** A second
        // hint line is not available here: `accounts_hint_dy` already sits in
        // the lower half of the 40 px of slack `ACCOUNTS_FOOTER_H` leaves, so a
        // line `LINE_H` below it lands ~3 px from the bottom edge and a 9 px
        // glyph would draw off-canvas. `Del` is also *wrong* on the offline row
        // — it cannot be removed — so making the term conditional fixes a
        // pre-existing lie rather than only adding a hint.
        accounts_hint_label(match accounts.third_button() {
            super::accounts::ThirdButton::Remove => "Enter select   Del remove   Esc back",
            super::accounts::ThirdButton::EditName => {
                "Enter select   Edit Name renames   Esc back"
            }
        }),
    ];
    if list_len == 1 {
        // Placed under the last row rather than under the title: the header band
        // is 33 px and holds a 9 px title, so there is no room for a subtitle
        // there. Row 1's top is the first free line, derived from the same two
        // values the rows are placed by rather than restated — and a one-row list
        // has nothing to scroll, so the unscrolled band top is the right anchor.
        labels.push(MenuLabel {
            text: "No accounts signed in - add one, or play offline".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: accounts_band_top() + ACCOUNTS_ITEM_H + 4.0,
            align: Align::Centre,
            colour: ACCOUNTS_DIM,
            scale: 1.0,
        });
    }
    // **The "Showing 1-5 of 9" counter is deliberately gone.** It was this screen's
    // stand-in for a scrollbar, and it existed only because the frame knew its own
    // window size while the draw had no bar to show. There is a real
    // `AbstractScrollArea` scrollbar now, drawn from `MenuFrame::list` by the same
    // `draw_scrollbar` the multiplayer list uses, so the counter would be a second
    // answer to the question the thumb already answers — and vanilla has no such
    // label on any selection list. Re-adding it would also mean re-deriving a
    // "shown" count that no longer exists: which rows are visible is now the real
    // canvas's answer, not the frame's.

    MenuFrame {
        rows,
        selected,
        vanilla: true,
        labels,
        // Not `message`: that is one unwrapped `TEXT_SCALE` line, and a keychain
        // or filesystem error carries an OS string of unknown length. See
        // `MenuNotice`.
        notice: accounts.save_error().map(accounts_save_error_notice),
        ..Default::default()
    }
}

/// Builds the account screen's frame while a sign-in is in flight: the URL to
/// open, the code to type if the flow has one, and a Cancel button.
///
/// **The URL is the [`MenuNotice`], not a label.** A loopback authorize URL is a
/// few hundred characters of query string with no whitespace in it, so it has to
/// wrap on character boundaries and be bounded to the content band — the same
/// requirement the failure message has, for the same reason.
///
/// The **code** is a label, and it is pre-clipped with [`clip`] rather than
/// carried whole. That is safe where the URL is not: `clip` measures at the
/// fixed-advance fallback font, whose 6 px per glyph is an upper bound on the
/// real proportional font, so a clip to half the row column is a conservative
/// bound in either font.
#[must_use]
pub(super) fn accounts_flow_frame(
    user_code: Option<&str>,
    verification_uri: Option<&str>,
    waiting: bool,
) -> MenuFrame<'static> {
    let mut labels = vec![accounts_title_label(ACCOUNTS_SIGN_IN_TITLE)];
    labels.push(accounts_band_label(
        if waiting {
            "Waiting for you to finish signing in...".to_string()
        } else {
            "Contacting Microsoft...".to_string()
        },
        0.0,
        LABEL,
    ));
    if let Some(code) = user_code {
        labels.push(accounts_band_label(
            format!("Then enter this code: {}", clip(code, ACCOUNTS_ROW_W * 0.5, 1.0)),
            2.0,
            LABEL,
        ));
    }
    if verification_uri.is_some() {
        labels.push(accounts_band_label(
            "Your browser was opened at this address:".to_string(),
            4.0,
            ACCOUNTS_DIM,
        ));
    }
    labels.push(accounts_hint_label(if waiting {
        "O reopen browser   C copy code   Esc cancel"
    } else {
        "Esc cancel"
    }));

    MenuFrame {
        rows: vec![MenuRow {
            label: "Cancel".to_string(),
            enabled: true,
            slot: Some(accounts_wide_button_slot()),
            ..Default::default()
        }],
        selected: 0,
        vanilla: true,
        labels,
        notice: verification_uri.map(|uri| {
            let mut notice = accounts_notice(uri.to_string(), ACCOUNTS_DIM);
            // Below the three label lines above, on the same `LINE_H` grid.
            notice.dy += LINE_H * 5.0;
            notice
        }),
        ..Default::default()
    }
}

/// The name field on the offline-name editor.
///
/// [`create_world::row_slot`](super::create_world::row_slot)'s `FIELD_W` of 200
/// and [`EDIT_BOX_H`], because that is the other screen in this shell with a real
/// typed field and the two should not be different sizes for no reason. The `dy`
/// is [`accounts_band_top`] plus two [`LINE_H`] lines, so the field sits under
/// the explanatory line above it on the same grid
/// [`accounts_band_label`] places every other content-band string on — derived,
/// not restated, per `CLAUDE.md`'s rule about a rect a gate restates.
fn accounts_name_field_slot() -> Slot {
    const FIELD_W: f32 = 200.0;
    Slot {
        origin: Origin::ScreenTop,
        dx: -(FIELD_W * 0.5),
        dy: accounts_band_top() + LINE_H * 2.0,
        w: FIELD_W,
        h: EDIT_BOX_H,
    }
}

/// Builds the offline-name editor: one [`super::edit_box::EditBox`] row, the
/// UUID the typed name would join under, and a Done button.
///
/// ## The UUID line is the point, not decoration
///
/// `crate::offline_identity`'s whole reason for existing is that the name **is**
/// the identity: an offline-mode server derives the account UUID from it, so
/// changing the name changes which player file the server opens. Showing
/// [`super::accounts::NameEditView::uuid`] live, off the *typed* value rather
/// than the saved one, is the only way that consequence is visible before the
/// player commits to it — and it is derived on every keystroke rather than
/// stored, exactly as `offline_uuid` is everywhere else.
///
/// ## Row order is a coupling, again
///
/// `super::accounts::NAME_EDIT_FIELD_ROW` then `NAME_EDIT_DONE_ROW`, and
/// `AccountsNav::click_name_edit_row` maps a click back through those same two
/// constants — `accounts_idle_frame`'s footer note applies here unchanged.
///
/// [`MenuFrame::selected`] is the Done row rather than the field: a field's own
/// highlight is its caret, drawn by `draw_edit_box` off the box's `focused` flag,
/// so pointing the row cursor at row 0 would draw a button highlight *behind* a
/// text field.
#[must_use]
pub(super) fn accounts_name_edit_frame(
    view: &super::accounts::NameEditView,
) -> MenuFrame<'static> {
    use super::accounts::{NAME_EDIT_DONE_ROW, NAME_EDIT_FIELD_ROW};

    let mut labels = vec![
        accounts_title_label(ACCOUNTS_EDIT_NAME_TITLE),
        accounts_band_label("Name this client joins under:".to_string(), 0.0, LABEL),
        // Below the field (two lines down for the label, plus the field's own
        // height expressed in `LINE_H` lines), so the two cannot overlap at any
        // canvas — the field's `dy` and this one come off the same expression.
        accounts_band_label(
            format!("Joins as {}", view.uuid),
            2.0 + (EDIT_BOX_H / LINE_H).ceil() + 1.0,
            ACCOUNTS_DIM,
        ),
        accounts_hint_label("Enter save   Esc cancel"),
    ];
    // A refusal is a *notice*, not a `message`, for `accounts_failed_frame`'s
    // reason: it is wrapped and bounded to the band rather than one unwrapped
    // uppercase line. `set_username` left the old name live, so this is the only
    // thing that tells the player nothing was saved.
    if view.error.is_none() {
        labels.push(accounts_band_label(
            "at most 16 characters, no spaces".to_string(),
            2.0 + (EDIT_BOX_H / LINE_H).ceil() + 2.0,
            ACCOUNTS_DIM,
        ));
    }

    let mut rows = vec![MenuRow::default(); 2];
    rows[NAME_EDIT_FIELD_ROW] = MenuRow {
        // The field's own text comes from the widget; `label` is carried for the
        // same reason `create_world::frame`'s field rows carry it.
        label: view.edit.value().to_string(),
        enabled: true,
        field: true,
        edit: Some(view.edit.clone()),
        slot: Some(accounts_name_field_slot()),
        ..Default::default()
    };
    rows[NAME_EDIT_DONE_ROW] = MenuRow {
        label: "Done".to_string(),
        enabled: true,
        slot: Some(accounts_wide_button_slot()),
        ..Default::default()
    };

    MenuFrame {
        rows,
        selected: NAME_EDIT_DONE_ROW,
        vanilla: true,
        labels,
        notice: view
            .error
            .as_ref()
            .map(|e| {
                let mut notice = accounts_notice(e.clone(), FG_BAD);
                // Under the UUID line, on the same `LINE_H` grid.
                notice.dy += LINE_H * (4.0 + (EDIT_BOX_H / LINE_H).ceil());
                notice
            }),
        ..Default::default()
    }
}

/// Builds the account screen's frame for a failed sign-in attempt.
///
/// **This is the frame the reported bug was in.** The message used to go into
/// [`MenuFrame::message`] — one `to_uppercase`d line, centred at [`TEXT_SCALE`]
/// with no wrap and no clip — so a reason built from a server's own response body
/// was both unreadably large and wider than the screen. It is now a
/// [`MenuNotice`]: wrapped in the draw's own font, broken inside an over-long
/// word, and clipped to as many lines as the content band holds.
#[must_use]
pub(super) fn accounts_failed_frame(message: &str) -> MenuFrame<'static> {
    MenuFrame {
        rows: vec![MenuRow {
            label: "Back to Accounts".to_string(),
            enabled: true,
            slot: Some(accounts_wide_button_slot()),
            ..Default::default()
        }],
        selected: 0,
        vanilla: true,
        labels: vec![
            accounts_title_label(ACCOUNTS_FAILED_TITLE),
            accounts_hint_label("Enter or Esc continues"),
        ],
        notice: Some(accounts_notice(message.to_string(), FG_BAD)),
        ..Default::default()
    }
}

