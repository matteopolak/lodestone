//! Pixel measurement: [`logical_canvas`]'s physical-to-logical conversion,
//! the bitmap font's advance/width/clip helpers, the generic row rects, and
//! the `ManageServerScreen` form's metrics and field rects.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

/// The logical canvas size [`geometry`] should lay its fixed pixel constants
/// into, given the real framebuffer size in physical pixels and the user's
/// `gui_scale` option (`0` = auto). This is the one function that fixes the
/// "menu draws half-size on Retina" report: it divides the framebuffer by the
/// effective integer [`crate::config::calculate_gui_scale`], exactly vanilla's
/// `Window.guiScaledWidth`/`Height` — so a fixed `ROW_W` comes out the same
/// *visual* size at any DPI, rather than shrinking as the physical framebuffer
/// grows. At `scale == 1` this is the identity (canvas == framebuffer), which
/// is why every `geometry`-calling test below, which passes a fixed size
/// directly, is unaffected by this existing.
#[must_use]
pub fn logical_canvas(gui_scale: u32, framebuffer_width: u32, framebuffer_height: u32) -> (f32, f32) {
    let scale = crate::config::calculate_gui_scale(gui_scale, framebuffer_width, framebuffer_height).max(1);
    (
        framebuffer_width as f32 / scale as f32,
        framebuffer_height as f32 / scale as f32,
    )
}

/// Per-glyph horizontal advance at `scale` (fixed advance: cell plus one column).
pub(super) fn advance(scale: f32) -> f32 {
    (GLYPH_W as f32 + 1.0) * scale
}

/// Pixel width of `s` at `scale`, legacy `§`+code pairs counted as zero-width.
///
/// This is the jar-less twin of `VanillaFont::width`, and it has to agree with
/// the jar-less *draw* (`hud::item_icon::ColourStream::text`), which consumes
/// those pairs rather than drawing them. Counting them here would over-measure by
/// two characters per code and push every centred label left of where it draws —
/// the same defect the proportional path had.
#[must_use]
pub fn text_px(s: &str, scale: f32) -> f32 {
    crate::hud::item_icon::text_w(s, scale)
}

/// Truncates `s` so it fits in `max_px` at `scale`, appending nothing (the font
/// has no ellipsis glyph). Returns a slice of `s`.
pub(super) fn clip(s: &str, max_px: f32, scale: f32) -> &str {
    let fits = (max_px / advance(scale)).floor().max(0.0) as usize;
    match s.char_indices().nth(fits) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Height of the row stack for `rows`.
fn row_height(row: &MenuRow) -> f32 {
    if row.favicon.is_some() || row.head.is_some() || !row.detail.is_empty() {
        LIST_ROW_H
    } else {
        BUTTON_H
    }
}

/// Which row of `frame` a logical-pixel cursor is over, or `None`.
///
/// **The one definition of "the row under the pointer",** called by `app`'s
/// `menu_row_at_in` for hover and clicks and by [`super::draw`] to decide whose
/// tooltip to show. It used to live only in `app`, which meant the draw could only
/// have got a *second* copy — and the two would then disagree about exactly the case
/// below, where a tooltip would appear for a row that cannot be clicked.
///
/// Rows and the footer share **one flat index space** and the first rect containing
/// the cursor wins, with the footer last, so a scrolling-list row that overhangs the
/// band would steal the footer button's clicks *and* its hover along the strip it
/// overhangs. The guard is vanilla's: `AbstractSelectionList.getEntryAtPosition`
/// tests the cursor against the list's own box before it walks the entries, so an
/// entry scrolled past the bottom cannot be hit where it would have painted.
///
/// Both halves are derived rather than restated — the band from `frame.list` through
/// `ListSpec::model`, the same two calls the draw's clip makes, and membership from
/// `MenuRow::is_scrolling_list_row`, the same call the draw makes to decide *whether*
/// to clip.
#[must_use]
pub fn menu_row_under(
    frame: &MenuFrame<'_>,
    cursor: (f32, f32),
    width: f32,
    height: f32,
) -> Option<usize> {
    let (lx, ly) = cursor;
    let band = frame
        .list
        .as_ref()
        .and_then(|spec| spec.model(height))
        .map(|list| (list.top(), list.bottom()));
    (0..frame.rows.len()).find(|&i| {
        if let Some((top, bottom)) = band
            && frame.rows[i].is_scrolling_list_row()
            && (ly < top || ly > bottom)
        {
            return false;
        }
        row_rect(&frame.rows, i, width, height)
            .is_some_and(|(rx, ry, rw, rh)| lx >= rx && lx <= rx + rw && ly >= ry && ly <= ry + rh)
    })
}

/// The pixel rect of row `i`, given the viewport. Public so the renderer, the
/// mouse hover and the click hit-test share one definition of where a row
/// actually is — `app.rs`'s `menu_row_at` calls exactly this.
///
/// A row carrying a [`MenuRow::slot`] is placed by vanilla's arithmetic; every
/// other row falls through to the centred stack. Keeping both behind this one
/// signature is deliberate: it is what let the two vanilla screens change layout
/// entirely without `app.rs`'s hit-test changing at all, so keyboard selection,
/// hover and clicks could not drift apart from the draw.
#[must_use]
pub fn row_rect(rows: &[MenuRow], i: usize, width: f32, height: f32) -> Option<(f32, f32, f32, f32)> {
    let row = rows.get(i)?;
    // A multiplayer-list entry (#396) is placed by `AbstractSelectionList`'s
    // arithmetic, which a `Slot` cannot express: `getRowLeft()` halves the canvas
    // width and the row width *separately* with integer division, so it is not
    // `anchor + dx` for any anchor. Answered here rather than in the caller for
    // this function's whole reason — `app.rs`'s hit-test reads it too, so a second
    // definition is how a click lands on a row the draw put somewhere else.
    //
    // #402: gated on `server_row_visible` first, so a row that is scrolled out
    // of the band or would overflow into the footer reports **no** rect at all,
    // rather than one nothing draws at. That is what keeps a click from landing
    // on a row that is not on screen — `menu_row_at`'s `find` simply keeps
    // scanning past a `None` the same way it already does past the end of
    // `rows`. Contrast the account-row arm below, which still has this gap.
    if let Some(view) = row.entry.as_ref() {
        if !server_row_visible(view.index, height, view.scroll) {
            return None;
        }
        return Some(server_row_rect(view.index, width, view.scroll));
    }
    // An account row (#66/#402) is placed the same way and for the same reason —
    // `floor(width / 2) - floor(305 / 2)` is two integer divisions, not
    // `anchor + dx`. Answered here so the draw and `app.rs`'s hit-test read one
    // definition.
    //
    // **The visibility gate is no longer the gap the comment above used to record.**
    // It became load-bearing rather than merely tidy when the account frame stopped
    // slicing its rows: `accounts_idle_frame` now emits *every* logical row and
    // positions them by pixel offset, exactly as the multiplayer list does, so
    // without this a click below a scrolled list would hit-test onto a row that is
    // nowhere near the cursor. Same shape as the arm above, and `menu_row_at`'s
    // `find` already scans past a `None`.
    if let Some(view) = row.account.as_ref() {
        if !accounts_row_visible(view.index, height, view.scroll) {
            return None;
        }
        return Some(accounts_row_rect(view.index, width, view.scroll));
    }
    // A world-list row (the save list) is placed the same way and for the same
    // reason again — `getRowLeft()` is `floor(width / 2) - floor(270 / 2)`, two
    // integer divisions rather than `anchor + dx`. The visibility gate is
    // load-bearing here rather than tidy: since #541 this list **scrolls**, so a
    // row scrolled out of the band must report no rect at all — otherwise a click
    // below the last visible row would land on a row that is nowhere near the
    // cursor, and (in the other direction) a focusable row could sit off-screen.
    if let Some(view) = row.world.as_ref() {
        // The **re-clamped** offset, not `view.scroll`: see
        // `world_list_scroll_for`. The draw reads the same function, so the rect a
        // click hits and the rect that was painted are one expression.
        let scroll = world_list_scroll_for(rows, height);
        if !world_list_row_visible(view.index, height, scroll) {
            return None;
        }
        return Some(world_list_row_rect(view.index, width, scroll));
    }
    // A tab-bar row (issue #564, and second consumer) is placed
    // by `MenuTabBar.arrangeElements`'s arithmetic, which a `Slot` cannot
    // express either — not because of an integer division like the three arms
    // above, but because the row's own *width* is a function of the canvas
    // (`layout::tab_bar_geometry`'s `min(400, width)` clamp), and `Slot::w` is
    // a fixed field. Answered here for this function's whole reason: the draw
    // and `app.rs`'s hit-test must read one definition of where a tab is.
    //
    // Resolved through `layout::tab_bar_row_rect` directly, off the row's own
    // `index`/`count`, rather than by calling into a screen module (this used
    // to hard-code `super::stats::tab_row_rect`) — that hard-code was exactly
    // what left Create New World's own tab bar with no generic geometry to
    // resolve against when it became this type's second consumer.
    if let Some(tab) = row.tab.as_ref() {
        return Some(super::layout::tab_bar_row_rect(tab.index, tab.count, width));
    }
    if let Some(slot) = row.slot {
        return Some(slot.resolve(width, height));
    }
    // Only the *other* unslotted rows count toward the centred stack's total
    // height and this row's offset within it. Without this filter, a frame
    // that mixes centred rows with vanilla-positioned ones (the account
    // list's scrollable rows plus its slotted action buttons) would have
    // every slotted row's height silently added into the centred group's
    // math even though that row is drawn somewhere else entirely — no
    // existing screen mixes the two kinds, so this could not be observed
    // before the account screen needed it.
    let total: f32 = rows
        .iter()
        .filter(|r| r.slot.is_none())
        .map(|r| row_height(r) + ROW_GAP)
        .sum::<f32>()
        .max(0.0)
        - ROW_GAP;
    // Centred vertically, but never above the title block.
    let top = ((height - total) * 0.5).max(110.0);
    let y = top
        + rows[..i]
            .iter()
            .filter(|r| r.slot.is_none())
            .map(|r| row_height(r) + ROW_GAP)
            .sum::<f32>();
    let w = ROW_W.min(width - 2.0 * PAD);
    Some(((width - w) * 0.5, y, w, row_height(row)))
}

/// An [`EditBox`] row's **box** height: vanilla's own 20
/// (`EditBox.java`, `Button.DEFAULT_HEIGHT`), taken off the *top* of the
/// 40 px [`LIST_ROW_H`] the row occupies so its `detail` hint still fits
/// underneath.
///
/// 20 rather than the whole row is not an arbitrary choice: it puts
/// [`EditBox::text_y`] — `y + floor((h - 8) / 2)` — at `y + 6`, which is exactly
/// where the pre-#395 draw put the label (`y + PAD`). The conversion therefore
/// leaves the *text* where it was and only changes the background from a flat
/// fill to vanilla's real `widget/text_field` nine-slice.
pub const EDIT_BOX_H: f32 = 20.0;

// -- vanilla's `ManageServerScreen` metrics ----------------------------------
//
// Vanilla's own manage-server-screen class
// — the add/edit-server form `JoinMultiplayerScreen`'s Add/Edit buttons open.
// Every number is transcribed from there, not measured off this pipeline's
// own output.

/// Every widget's width: the two `EditBox`es and the three buttons below them
/// are all `200` (`:33-38,43,55,58`).
const MANAGE_SERVER_W: f32 = 200.0;
/// Every widget's x, relative to [`Origin::ScreenTop`]: `width / 2 - 100`
/// (same lines).
const MANAGE_SERVER_X: f32 = -100.0;
/// The name field's y (`:33`).
const MANAGE_SERVER_NAME_Y: f32 = 66.0;
/// The address field's y (`:38`).
const MANAGE_SERVER_ADDRESS_Y: f32 = 106.0;
/// The Resource Packs button's y, as an offset from [`Origin::TitleTop`]'s own
/// `height / 4 + 48` anchor: `height / 4 + 72` is `+ 24` from there (`:43`).
/// Reusing `TitleTop` rather than a second `height / 4` origin is the same
/// choice [`death_slot`] already made, for the same reason named there.
const MANAGE_SERVER_PACK_DY: f32 = 24.0;
/// Done's y: `height / 4 + 114` is `+ 66` from [`Origin::TitleTop`] (`:55`).
const MANAGE_SERVER_DONE_DY: f32 = 66.0;
/// Cancel's y: `height / 4 + 138` is `+ 90` from [`Origin::TitleTop`] (`:58`).
const MANAGE_SERVER_CANCEL_DY: f32 = 90.0;
/// Where this screen's title is drawn: vanilla `Screen`'s own generic
/// `drawCenteredString(title, width / 2, 20, …)` fallback — `ManageServerScreen`
/// overrides neither `render` nor `renderBackground`/`addTitle`, so its title
/// draws wherever the base `Screen` puts one, same as every simple dialog that
/// does not build a `HeaderAndFooterLayout` of its own.
pub(super) const MANAGE_SERVER_TITLE_Y: f32 = 20.0;

/// One [`super::Screen::ServerEdit`] widget's [`Slot`] — vanilla's rects at
/// `ManageServerScreen.java`, read out of the constants above rather
/// than resolved by hand, so a click, a hover and the draw cannot disagree.
/// Row indices are [`super::nav::NAME_FIELD`] and its siblings.
#[must_use]
pub(super) fn manage_server_slot(row: usize) -> Slot {
    use super::nav::{ADDRESS_FIELD, DONE_ROW, NAME_FIELD, RESOURCE_PACK_ROW};
    match row {
        NAME_FIELD => Slot {
            origin: Origin::ScreenTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_NAME_Y,
            w: MANAGE_SERVER_W,
            h: EDIT_BOX_H,
        },
        ADDRESS_FIELD => Slot {
            origin: Origin::ScreenTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_ADDRESS_Y,
            w: MANAGE_SERVER_W,
            h: EDIT_BOX_H,
        },
        RESOURCE_PACK_ROW => Slot {
            origin: Origin::TitleTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_PACK_DY,
            w: MANAGE_SERVER_W,
            h: WIDGET_H,
        },
        DONE_ROW => Slot {
            origin: Origin::TitleTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_DONE_DY,
            w: MANAGE_SERVER_W,
            h: WIDGET_H,
        },
        // `CANCEL_ROW`, and any row past it: the caller never asks for one
        // this screen does not have, so the match stays a `usize` rather than
        // an enum to share `Cell`-free indices with `EditForm`'s focus ids.
        _ => Slot {
            origin: Origin::TitleTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_CANCEL_DY,
            w: MANAGE_SERVER_W,
            h: WIDGET_H,
        },
    }
}

/// The rect an [`EditBox`] row's box occupies, as distinct from the whole row's.
///
/// Derived from [`row_rect`] rather than restated, so the field, the row fill,
/// `app.rs`'s hit-test and [`super::nav::EditForm`]'s seed geometry cannot drift
/// apart — `CLAUDE.md`'s "derive layout from the same expression the draw uses".
#[must_use]
pub fn field_rect(
    rows: &[MenuRow],
    i: usize,
    width: f32,
    height: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, _) = row_rect(rows, i, width, height)?;
    Some((x, y, w, EDIT_BOX_H))
}

/// The two [`super::Screen::ServerEdit`] field rects at a given canvas —
/// vanilla's own `ManageServerScreen.java` rects, through
/// [`manage_server_slot`].
///
/// Exists so [`super::nav::EditForm::adding`] can seed its two `EditBox`es'
/// geometry before any frame exists — arrow navigation between them is
/// geometric and `displayPos` scrolling is measured against the width, so a
/// freshly-constructed box needs real bounds immediately. Reads the same
/// [`manage_server_slot`] the real per-frame rows resolve through (via their
/// own `slot`), rather than a second computation that could drift from it.
#[must_use]
pub fn field_row_rects(width: f32, height: f32) -> [(f32, f32, f32, f32); 2] {
    use super::nav::{ADDRESS_FIELD, NAME_FIELD};
    [
        manage_server_slot(NAME_FIELD).resolve(width, height),
        manage_server_slot(ADDRESS_FIELD).resolve(width, height),
    ]
}

