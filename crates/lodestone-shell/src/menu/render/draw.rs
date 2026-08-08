//! The draw layer: [`MenuGeometry`], [`geometry`]/[`build`], the per-entry
//! and per-widget draws, and the `Quads` vertex-stream builder they all
//! write into.
//!
//! `Quads` stays in the same file as its callers deliberately: they reach its
//! `verts`/`sprites`/`atlas`/`font` fields directly, so splitting it out
//! would mean widening four fields and nine methods to `pub(super)` for no
//! structural gain.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;
use super::account_screen::{ACCOUNTS_DETAIL_Y, ACCOUNTS_DIM, ACCOUNTS_HEAD_ICON, ACCOUNTS_ROW_W, ACCOUNTS_SELECTION_FILL, ACCOUNTS_SPACING, ACCOUNTS_TEXT_GAP};
use super::frame::notice_lines;
use super::world_list::WORLD_LIST_ROW_W;
use super::measure::{advance, clip};
use super::renderer::FLOATS_PER_VERTEX;
use super::world_list::{WORLD_LIST_DIM, WORLD_LIST_SELECTION_FILL};
use super::server_list::{SERVER_ENTRY_BAD, SERVER_ENTRY_DIM, SERVER_ENTRY_ICON, SERVER_ENTRY_INCOMPATIBLE, SERVER_ENTRY_MOTD_INSET, SERVER_ENTRY_MOTD_LINES, SERVER_ENTRY_MOTD_Y, SERVER_ENTRY_SPACING, SERVER_ENTRY_TEXT_GAP, SERVER_ICON_DARKEN, SERVER_JOIN_SPRITES, SERVER_LIST_ROW_W, SERVER_LIST_SELECTION_FILL, SERVER_MOVE_DOWN_SPRITES, SERVER_MOVE_UP_SPRITES, SERVER_UNKNOWN_ICON};

/// The multiplayer list's "who's online" tooltip fill — `tooltip/background.png`'s
/// centre pixel, `0xF0100010`: translucent near-black (16, 0, 16, 240). The sprite
/// is a 1-bit indexed, 9 px-border nine-slice with nothing but this in its opaque
/// centre, so one flat quad is the whole of it. Decoded straight out of the 26.2
/// `client.jar`. `pub(super)` so the draw's gate can assert the box by colour.
pub(super) const TOOLTIP_BG: [f32; 4] = [16.0 / 255.0, 0.0, 16.0 / 255.0, 240.0 / 255.0];
/// Inset of a resource-pack row's content box from the entry's own edges (issue
/// #415) — `AbstractSelectionList.Entry.CONTENT_PADDING` (`:436`), the same 2 px
/// every other selection list here insets by. It is what makes a 36 px row's
/// content box exactly 32 px tall, which is exactly
/// `TransferableSelectionList.PackEntry.ICON_SIZE`.
pub(super) const PACK_ROW_PAD: f32 = 2.0;
/// `PackEntry.ICON_SIZE` (`TransferableSelectionList.java:112`) — and the [`ICON`]
/// 32 the account and server lists already draw their mosaics at, so there is one
/// mosaic size on this pass and not three.
pub(super) const PACK_ICON: f32 = 32.0;
/// `nameWidget.setPosition(getContentX() + 32 + 2, …)` /
/// `descriptionWidget.setPosition(getContentX() + 32 + 2, …)` (`:214,217`) — the
/// icon column plus a 2 px gutter, which both text lines measure from.
pub(super) const PACK_TEXT_DX: f32 = PACK_ICON + 2.0;
/// The name line's y within the content box: `getContentY() + 1` (`:214`).
pub(super) const PACK_NAME_DY: f32 = 1.0;
/// The description block's y within the content box: `getContentY() + 12`
/// (`:217`).
pub(super) const PACK_DESC_DY: f32 = 12.0;
/// `PackEntry.MAX_DESCRIPTION_WIDTH_PIXELS` (`:111`), which vanilla applies to
/// **both** widgets' `setMaxWidth` (`:213,216`).
///
/// Vanilla subtracts a further 6 when its own list is scrollable, because its
/// scrollbar sits at `getRight() - scrollbarWidth()` — *inside* the 200 px list.
/// This screen's bar does not: both columns share one [`super::packs::BAND_W`]
/// band, so the bar is outside either column and there is nothing for the text to
/// run under. Deliberately unported for that reason, not overlooked.
const PACK_TEXT_MAX_W: f32 = 157.0;
/// `descriptionWidget.setMaxRows(2)` (`:127`).
const PACK_DESC_ROWS: usize = 2;
/// The description's colour: `Style.EMPTY.withColor(-8355712)` (`:125,152`) —
/// `0x808080`, vanilla's flat mid-grey, not this pass's own [`FG_DIM`].
pub(super) const PACK_ENTRY_DIM: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// The selected row's interior, `-16777216` — opaque black inside the 1 px
/// outline (`AbstractSelectionList.java:363-370`), exactly as the server, account
/// and world lists draw theirs.
const PACK_SELECTION_FILL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// The hovered row's icon dim, `fill(…, -1601138544)`
/// (`TransferableSelectionList.java:156`) — `0xA0909090`, the same translucent
/// grey the multiplayer list puts under its own hover arrows, cited here from its
/// own call site rather than shared with it.
pub(super) const PACK_ICON_DARKEN: [f32; 4] = [144.0 / 255.0, 144.0 / 255.0, 144.0 / 255.0, 160.0 / 255.0];
/// `SELECT_SPRITE` / `SELECT_HIGHLIGHTED_SPRITE` (`:24-25`) — the overlay an
/// **Available** row shows on hover. `pub(super)` so the real-pack sprite gate can
/// assert the ids exist.
pub(super) const PACK_SELECT_SPRITES: (&str, &str) = (
    "transferable_list/select",
    "transferable_list/select_highlighted",
);
/// `UNSELECT_SPRITE` / `UNSELECT_HIGHLIGHTED_SPRITE` (`:26-27`) — a removable
/// **Selected** row's overlay.
pub(super) const PACK_UNSELECT_SPRITES: (&str, &str) = (
    "transferable_list/unselect",
    "transferable_list/unselect_highlighted",
);
/// The fallback pack icon: vanilla's `PackSelectionScreen.DEFAULT_ICON`
/// (`PackSelectionScreen.java:67`), `textures/misc/unknown_pack.png`, blitted for
/// any pack that ships no readable `pack.png` — which is every built-in row and
/// most hand-made packs.
///
/// A **loose** texture like `misc/unknown_server`, so it reaches the atlas through
/// [`crate::resources::MENU_TEXTURES`] rather than the `gui/sprites/**` glob.
pub(super) const PACK_UNKNOWN_ICON: &str = "misc/unknown_pack";
/// `tooltip/frame.png`'s top bar and the light end of its side gradient —
/// (80, 0, 255, 80).
const TOOLTIP_FRAME_TOP: [f32; 4] = [80.0 / 255.0, 0.0, 1.0, 80.0 / 255.0];
/// `tooltip/frame.png`'s bottom bar and the dark end of its side gradient —
/// (40, 0, 127, 80).
const TOOLTIP_FRAME_BOTTOM: [f32; 4] = [40.0 / 255.0, 0.0, 127.0 / 255.0, 80.0 / 255.0];
/// `TooltipRenderUtil.PADDING` (`TooltipRenderUtil.java:14`) — the text's inset
/// from the tooltip's fill edges: 3 px each side, so a `w×h` content box carries
/// a `(w+6)×(h+6)` fill.
const TOOLTIP_PAD: f32 = 3.0;
/// `TooltipRenderUtil.MOUSE_OFFSET` (`TooltipRenderUtil.java:11`) — the content
/// box's top-left starts this far right of, and this far above, the cursor.
const TOOLTIP_MOUSE_OFFSET: f32 = 12.0;
/// `ClientTextTooltip`'s line box: vanilla's 9 px `Font.lineHeight` plus a 1 px
/// drop-shadow overhang (`ClientTextTooltip.java:20-22`), and the +2 interline
/// gap vanilla adds after the first line brings that second line's offset back to
/// the same 12 as [`TOOLTIP_MOUSE_OFFSET`]. The first line starts at `y`; a
/// 1-line tooltip is 8 px tall, an `n`-line one `10n`.
const TOOLTIP_LINE_H: f32 = 10.0;

/// Both vertex streams one menu frame produces.
///
/// Two streams because the buttons are **textured** (nine-slice sprites off the
/// GUI atlas) while everything else — backdrops, row fills, text — is a flat
/// coloured quad. They need different pipelines, so they cannot share a buffer.
///
/// `backdrop_floats` is the split the caller draws the sprite stream *between*:
/// the full-screen backdrop first, then every sprite, then the rest of the
/// colour stream. That ordering is load-bearing — a button's label is on the
/// colour stream, so drawing all colour before all sprites would bury every
/// label under the button it belongs to.
#[derive(Debug, Clone, Default)]
pub struct MenuGeometry {
    /// Interleaved `[x, y, r, g, b, a]` in NDC, two triangles per quad.
    pub colour: Vec<f32>,
    /// How many floats at the head of [`Self::colour`] are the full-screen
    /// backdrop quad.
    pub backdrop_floats: usize,
    /// Interleaved `[x, y, u, v, r, g, b, a]` in NDC + atlas UVs.
    pub sprite: Vec<f32>,
}

/// Builds the coloured-quad stream for one menu frame with no atlas and no
/// vanilla font — the jar-less path, and the shape every layout test uses.
///
/// Pure: no GPU, no state. Returns interleaved `[x, y, r, g, b, a]` in NDC.
#[must_use]
pub fn geometry(frame: &MenuFrame<'_>, width: f32, height: f32) -> Vec<f32> {
    build(frame, None, None, width, height).colour
}

/// Builds both vertex streams for one menu frame.
///
/// `atlas` supplies the real `widget/button*` nine-slice sprites, the icon
/// buttons' sprites and the title logo; `font` supplies vanilla's proportional
/// text. Both are `Option` and both degrade the same way every other vanilla
/// asset does in this crate: flat coloured button fills and the fixed-advance
/// 5×7 debug font, which is what a jar-less or headless run gets. Pure — no GPU.
#[must_use]
pub fn build(
    frame: &MenuFrame<'_>,
    atlas: Option<&GuiAtlas>,
    font: Option<&VanillaFont>,
    width: f32,
    height: f32,
) -> MenuGeometry {
    let mut b = Quads::new(width, height);
    b.atlas = atlas;
    b.font = font;
    let backdrop = if frame.overlay { OVERLAY_BG } else { BG };
    b.rect(0.0, 0.0, width, height, backdrop);
    let backdrop_floats = b.verts.len();

    if frame.logo {
        // Vanilla's `LogoRenderer`: the wordmark centred at y=30, the edition
        // strip centred under it overlapping by 7 px.
        b.sprite("title/minecraft", (width * 0.5).floor() - 128.0, LOGO_Y, LOGO_W, LOGO_H, LABEL);
        b.sprite(
            "title/edition",
            (width * 0.5).floor() - 64.0,
            EDITION_Y,
            EDITION_W,
            EDITION_H,
            LABEL,
        );
    }

    if frame.vanilla {
        for label in &frame.labels {
            let (ax, ay) = label.origin.anchor(width, height);
            let tw = b.text_width(&label.text, label.scale);
            let x = match label.align {
                Align::Left => ax + label.dx,
                Align::Centre => (ax + label.dx - tw * 0.5).floor(),
                Align::Right => ax + label.dx - tw,
            };
            b.text(&label.text, x, ay + label.dy, label.scale, label.colour);
        }
        // The loading screen's progress bar (issue #449), drawn after the labels
        // so it sits over the backdrop and under nothing. Vanilla's
        // `LevelLoadingScreen` geometry: 200x2, centred, black track, green
        // fill; the fill is `round(fraction * 200)` so a partial column shows as
        // a whole pixel rather than a sub-pixel smear.
        if let Some(progress) = frame.progress {
            let bar_x = (width * 0.5 - PROGRESS_BAR_W * 0.5).floor();
            let bar_y = (height * 0.5 + progress.dy).floor();
            b.rect(bar_x, bar_y, PROGRESS_BAR_W, PROGRESS_BAR_H, PROGRESS_BAR_BG);
            let filled = (progress.fraction * PROGRESS_BAR_W).round();
            if filled > 0.0 {
                b.rect(bar_x, bar_y, filled, PROGRESS_BAR_H, PROGRESS_BAR_FG);
            }
        }
    } else {
        // The row-stack screens' own centred title block.
        let tw = text_px(frame.title, TITLE_SCALE);
        b.text(frame.title, (width - tw) * 0.5, 40.0, TITLE_SCALE, FG);
        if !frame.subtitle.is_empty() {
            let sw = text_px(frame.subtitle, TEXT_SCALE);
            b.text(
                frame.subtitle,
                (width - sw) * 0.5,
                40.0 + GLYPH_H as f32 * TITLE_SCALE + 8.0,
                TEXT_SCALE,
                FG_DIM,
            );
        }
    }

    // A wrapped, bounded block of body text (see `MenuNotice`). Drawn here rather
    // than through `frame.labels` because it is the one piece of menu text whose
    // *content* is not ours — it can carry a server's own response body — so it
    // has to be wrapped in the font this draw measures with and clipped to a rect
    // the layout sizes. Unconditional rather than inside the `frame.vanilla`
    // branch above: nothing about it depends on which layout mode a screen is in.
    if let Some(notice) = frame.notice.as_ref() {
        let (nx, ny, nw, _) = notice_rect(notice, width, height);
        let lines = wrap_bounded(&b, &notice.text, nw, notice_lines(notice, width, height));
        for (i, line) in lines.iter().enumerate() {
            let lw = b.text_width(line, 1.0);
            b.text(
                line,
                (nx + (nw - lw) * 0.5).floor(),
                ny + LINE_H * i as f32,
                1.0,
                notice.colour,
            );
        }
    }

    // The multiplayer list's scrollbar (the owner's first report: "server list
    // needs a scrollbar"). Drawn *before* the rows so a row can never be painted
    // over by the bar — vanilla's order is the reverse (`extractScrollbar` runs
    // last, `AbstractSelectionList.java:216`) because it scissors the rows to the
    // band first; the bar sits outside the rows here either way
    // (`scrollBarX() = getRowRight() + 8`), so the order is not observable.
    //
    // Every input is derived from the same expressions the rows are placed by —
    // `server_list_block().content_top`, `SERVER_LIST_FOOTER_H`,
    // `SERVER_LIST_ITEM_H`, `server_row_left` — rather than restated, which is the
    // rule a HUD gate here broke by hardcoding an anchor the draw computed.
    // **Generic since the `ListSpec` hook landed.** This used to call
    // `server_scroll_list` *by name*, which made it the multiplayer list's scrollbar
    // rather than "the active screen's" — so a second screen adopting `ScrollList`
    // got correct geometry, green tests and no bar at all. Any screen that returns a
    // spec from `MenuNav::active_list` is drawn here now, and `row_right` comes off
    // the spec's own `getRowLeft()` expression rather than from a screen-specific
    // constant, so the bar cannot sit somewhere its rows are not.
    let active_list = frame.list.as_ref().and_then(|spec| {
        spec.model(height)
            .map(|list| (list, spec.row_right(width)))
    });
    if let Some((list, row_right)) = active_list.as_ref() {
        draw_scrollbar(&mut b, list, *row_right);
    }

    // The active list's own row text, clipped to its band (see
    // `MenuFrame::list_labels`). Drawn through the same `Origin::anchor` →
    // `Align` path as `frame.labels` above rather than a second copy of it, so
    // a screen's list rows and its title cannot drift apart in how they place
    // text; the *only* difference is the scissor.
    if !frame.list_labels.is_empty() {
        let band = active_list
            .as_ref()
            .map(|(list, _)| (list.top(), list.height()));
        let mut draw_list_labels = |b: &mut Quads<'_>| {
            for label in &frame.list_labels {
                let (ax, ay) = label.origin.anchor(width, height);
                let tw = b.text_width(&label.text, label.scale);
                let x = match label.align {
                    Align::Left => ax + label.dx,
                    Align::Centre => (ax + label.dx - tw * 0.5).floor(),
                    Align::Right => ax + label.dx - tw,
                };
                b.text(&label.text, x, ay + label.dy, label.scale, label.colour);
            }
        };
        match band {
            // Full canvas width: a list label is positioned from its own
            // `Origin`, which for a two-column screen straddles the centre, so
            // clipping horizontally to `row_w` would crop the value column.
            // The band is vertical; that is the whole of what must be clipped.
            Some((top, height_px)) => {
                b.with_clip(0.0, top, width, height_px, |b| draw_list_labels(b));
            }
            None => draw_list_labels(&mut b),
        }
    }

    // The multiplayer list's "who's online" tooltip, hoisted out of the row loop:
    // only one row can be hovered at a time, so the last request wins, and the
    // tooltip itself is drawn last of all, after the footer, so it sits over
    // everything (vanilla's `render` draws tooltips after `renderables`, too).
    let mut pending_tooltip: Option<Vec<String>> = None;
    for (i, row) in frame.rows.iter().enumerate() {
        // A multiplayer-list entry (#396) is neither a button nor a field: it is
        // an `ObjectSelectionList` row with a favicon, two text columns, a status
        // sprite and a quadrant hover overlay. Tested before `slot` because it
        // carries none — `row_rect` places it from `entry.index`.
        if row.entry.is_some() {
            // Clipped to the list's band — vanilla's `enableScissor` around
            // `extractListItems` (`AbstractSelectionList.java:212-214`, `:242-249`).
            //
            // **This is what makes the clip real.** `Quads::with_clip` landed with
            // no caller, and an unexercised clip is worse than none: it reads as
            // "clipping is handled" while a row still paints over the footer. It
            // also removes the reason `server_row_visible` had to reject a row that
            // was merely *partly* outside the band — with a clip, a straddling row
            // is cut rather than dropped, which is the precondition for a
            // pixel-granular offset ever looking right.
            //
            // The rect is the band `server_scroll_list` derived, not a restated
            // one, so the clip and the row placement cannot disagree.
            //
            // The entry draw also reports the tooltip request — whether the cursor
            // is over *this* row's status text — which escapes the clip the same
            // way the row did: a tooltip near the bottom of the list must not be
            // scissored to the band, or it would vanish mid-screen.
            match active_list.as_ref() {
                Some((list, _)) => {
                    let (bx, bw) = (server_row_left(width), SERVER_LIST_ROW_W);
                    let (by, bh) = (list.top(), list.height());
                    let mut tooltip = None;
                    b.with_clip(bx, by, bw, bh, |b| {
                        tooltip = draw_server_entry(b, &frame.rows, i, width, height, frame.cursor);
                    });
                    if let Some(lines) = tooltip {
                        pending_tooltip = Some(lines);
                    }
                }
                None => {
                    if let Some(lines) =
                        draw_server_entry(&mut b, &frame.rows, i, width, height, frame.cursor)
                    {
                        pending_tooltip = Some(lines);
                    }
                }
            }
            continue;
        }
        // An account row (#66/#402) is the same kind of thing one screen over: a
        // 36 px selection-list entry with a head icon and two small text columns,
        // not a button. Tested before `slot` for the same reason — it carries
        // none.
        if row.account.is_some() {
            // Clipped to the band, exactly as a server entry is. This became
            // *required* rather than tidy when the account list went pixel-granular:
            // `accounts_row_visible` is a partial-overlap test now, so a row
            // straddling the band's bottom edge is the normal case at an
            // intermediate offset, and without the clip it would paint over the four
            // footer buttons. The rect is the band the spec derived, not a restated
            // one, so the clip and the row placement cannot disagree.
            match active_list.as_ref() {
                Some((list, row_right)) => {
                    let bw = ACCOUNTS_ROW_W;
                    let (by, bh) = (list.top(), list.height());
                    b.with_clip(row_right - bw, by, bw, bh, |b| {
                        draw_account_entry(b, &frame.rows, i, width, height);
                    });
                }
                None => draw_account_entry(&mut b, &frame.rows, i, width, height),
            }
            continue;
        }
        // A world-list row (the save list, #468's reading 2) is the third of the
        // same kind: a 36 px selection-list entry with an icon column and three
        // text lines, not a button. Tested before `slot` for the same reason — it
        // carries none.
        //
        // **Clipped to the band since #541, and the clip is required rather than
        // tidy.** This comment used to say the opposite, and said why: with no
        // scroll model `world_list_row_visible` rejected every row that was not
        // *wholly* inside the band, so there was nothing left to cut — and it
        // ended "if scrolling lands, the clip has to land with it — a
        // pixel-granular offset without one paints over the footer buttons."
        // Scrolling landed; this is that clip. At any offset that is not a
        // multiple of 36 a row straddles the band's bottom edge, which is the
        // normal case and not an edge one.
        //
        // The rect is the band the spec derived, not a restated one, so the clip
        // and the row placement cannot disagree.
        if row.world.is_some() {
            match active_list.as_ref() {
                Some((list, _)) => {
                    let (bx, bw) = (world_list_row_left(width), WORLD_LIST_ROW_W);
                    let (by, bh) = (list.top(), list.height());
                    b.with_clip(bx, by, bw, bh, |b| {
                        draw_world_entry(b, &frame.rows, i, width, height);
                    });
                }
                None => draw_world_entry(&mut b, &frame.rows, i, width, height),
            }
            continue;
        }
        if let Some(slot) = row.slot {
            // **A slotted row that is a list entry is clipped to the band too.**
            // The three list screens above are clipped because their rows are
            // `MenuRow::entry`/`account`/`world`; every settings-tree list draws
            // its rows as *slotted widgets* instead, so they fell through to the
            // unclipped path below and a row scrolled past the band's bottom
            // painted over the footer. That is the overlap a player reported on the
            // settings screen — see `Origin::is_scrolling_list_row`, which is the
            // predicate that decides, and which deliberately excludes the footer,
            // the title and `OptionsScreen`'s own grid (clipping *those* to the
            // band would erase them).
            //
            // The band is the same `ListSpec::model` the scrollbar is drawn from,
            // so the clip and the rows cannot disagree; horizontal extent is the
            // full canvas for `list_labels`' reason — a two-column settings row
            // straddles the centre and cropping to `row_w` would cut the value
            // column.
            let clip = slot
                .origin
                .is_scrolling_list_row()
                .then(|| active_list.as_ref().map(|(list, _)| (list.top(), list.height())))
                .flatten();
            // A vanilla-positioned row can be a **text field** rather than a
            // button: `Screen::WorldSelect`'s search box is placed by the header
            // layout's arithmetic like every other widget on that screen, and
            // drawn as an `EditBox`. Checked before `draw_widget` because the two
            // draws are mutually exclusive — a field is not a button with text in
            // it, and `EditBox` has its own sprite set and its own predicate (see
            // `draw_edit_box`).
            if let Some(edit) = row.edit.as_ref() {
                if let Some((x, y, w, h)) = row_rect(&frame.rows, i, width, height) {
                    match clip {
                        Some((top, band_h)) => b.with_clip(0.0, top, width, band_h, |b| {
                            draw_edit_box(b, edit, x, y, w, h);
                        }),
                        None => draw_edit_box(&mut b, edit, x, y, w, h),
                    }
                }
                continue;
            }
            let selected = i == frame.selected;
            let hovered = frame.hovered == Some(i);
            // A **resource-pack entry** (issue #415) is a selection-list row, not a
            // button: a 32×32 icon, a name, and up to two description lines. It is
            // tested here rather than before `slot` — unlike the three lists above —
            // because its rect *is* the slot; see `MenuRow::pack`.
            //
            // The reported bug this branch fixes: without it a pack row fell through
            // to `draw_widget` and came out as a button with a centred label, its
            // icon and description computed by `packs::frame` and then discarded.
            if row.pack.is_some() {
                let cursor = frame.cursor;
                match clip {
                    Some((top, band_h)) => b.with_clip(0.0, top, width, band_h, |b| {
                        draw_pack_entry(b, &frame.rows, i, width, height, selected, cursor);
                    }),
                    None => {
                        draw_pack_entry(&mut b, &frame.rows, i, width, height, selected, cursor);
                    }
                }
                continue;
            }
            match clip {
                Some((top, band_h)) => b.with_clip(0.0, top, width, band_h, |b| {
                    draw_widget(b, &frame.rows, i, width, height, selected, hovered);
                }),
                None => draw_widget(
                    &mut b,
                    &frame.rows,
                    i,
                    width,
                    height,
                    selected,
                    hovered,
                ),
            }
            continue;
        }
        let Some((x, y, w, h)) = row_rect(&frame.rows, i, width, height) else {
            continue;
        };
        let selected = i == frame.selected;
        // A row carrying a live `EditBox` (#395) draws through the widget: it
        // owns the caret, the selection and the horizontal scroll, none of which
        // are derivable from a `MenuRow`. Its `detail` hint still draws
        // underneath, at the same offset the pre-widget path used.
        if let Some(edit) = row.edit.as_ref() {
            let (fx, fy, fw, fh) =
                field_rect(&frame.rows, i, width, height).unwrap_or((x, y, w, EDIT_BOX_H));
            draw_edit_box(&mut b, edit, fx, fy, fw, fh);
            if !row.detail.is_empty() {
                let room = (fw - 2.0 * PAD).max(0.0);
                let detail = clip(&row.detail, room, SMALL_SCALE);
                let colour = if row.detail_is_error { FG_BAD } else { FG_DIM };
                b.text(
                    detail,
                    fx + PAD,
                    fy + fh + 3.0,
                    SMALL_SCALE,
                    colour,
                );
            }
            continue;
        }
        let fill = if row.field {
            FIELD_BG
        } else if selected {
            ROW_SEL
        } else if row.enabled {
            ROW_BG
        } else {
            ROW_OFF
        };
        b.rect(x, y, w, h, fill);
        // A 2 px selection border, so the highlight survives a screenshot even
        // where the fill difference is subtle.
        if selected {
            b.outline(x, y, w, h, 2.0, FG);
        }

        let mut text_x = x + PAD;
        if let Some(icon) = row.favicon.as_ref().or(row.head.as_ref()) {
            let iy = y + (h - ICON) * 0.5;
            b.mosaic(icon, text_x, iy, ICON);
            text_x += ICON + PAD;
        }
        let label_room = (x + w - PAD) - text_x - text_px(&row.trailing, SMALL_SCALE) - PAD;
        let label = clip(&row.label, label_room.max(0.0), TEXT_SCALE);
        let label_y = if row.detail.is_empty() {
            y + (h - GLYPH_H as f32 * TEXT_SCALE) * 0.5
        } else {
            y + PAD
        };
        b.text(label, text_x, label_y, TEXT_SCALE, FG);
        if row.field && selected {
            // Caret: a solid block one advance past the text.
            b.rect(
                text_x + text_px(label, TEXT_SCALE),
                label_y,
                TEXT_SCALE,
                GLYPH_H as f32 * TEXT_SCALE,
                FG,
            );
        }
        if !row.detail.is_empty() {
            let dy = label_y + GLYPH_H as f32 * TEXT_SCALE + 3.0;
            let detail = clip(&row.detail, label_room.max(0.0), SMALL_SCALE);
            let colour = if row.detail_is_error { FG_BAD } else { FG_DIM };
            b.text(detail, text_x, dy, SMALL_SCALE, colour);
        }
        if !row.trailing.is_empty() {
            let tx = x + w - PAD - text_px(&row.trailing, SMALL_SCALE);
            b.text(
                &row.trailing,
                tx,
                y + (h - GLYPH_H as f32 * SMALL_SCALE) * 0.5,
                SMALL_SCALE,
                FG_DIM,
            );
        }
    }

    // Message and footer, bottom-up. Not on a vanilla screen: vanilla has no
    // key-hint footer, and reproducing its layout means reproducing what it
    // does *not* draw as well.
    if !frame.vanilla {
        let mut fy = height - 12.0 - GLYPH_H as f32 * SMALL_SCALE;
        for line in frame.footer.iter().rev() {
            let lw = text_px(line, SMALL_SCALE);
            b.text(line, (width - lw) * 0.5, fy, SMALL_SCALE, FG_DIM);
            fy -= GLYPH_H as f32 * SMALL_SCALE + 4.0;
        }
        if let Some(msg) = &frame.message {
            let mw = text_px(msg, TEXT_SCALE);
            b.text(
                msg,
                (width - mw) * 0.5,
                fy - GLYPH_H as f32 * TEXT_SCALE,
                TEXT_SCALE,
                FG_BAD,
            );
        }
    }

    // The "who's online" tooltip, drawn last so it sits over every row and the
    // footer. Vanilla shows one on hover regardless of the row's own hover state
    // (its trigger is geometric — over the status text), so the only gates here
    // are "there are lines" and "the cursor is somewhere"; which row the cursor is
    // over was already decided by `draw_server_entry`.
    if let Some(lines) = pending_tooltip
        && let Some(at) = frame.cursor
    {
        draw_tooltip(&mut b, &lines, at, width, height);
    }

    MenuGeometry {
        colour: b.verts,
        backdrop_floats,
        sprite: b.sprites,
    }
}

/// Draws one multiplayer-list row: the selection outline, the favicon, the name,
/// up to two wrapped MOTD lines, the right-aligned status column, the status
/// sprite, and — when the cursor is inside the row — the join/move-up/move-down
/// overlay on the icon.
///
/// Mirrors `ServerSelectionList.OnlineServerEntry.extractContent`
/// (`ServerSelectionList.java:267-397`) plus `AbstractSelectionList.extractItem`'s
/// selection pass (`:354-370`), and it **decides nothing**: which sprite, which
/// colour and which arrows apply are all resolved into [`ServerEntryView`] by
/// [`server_list_frame`]. What it does own is everything canvas-dependent — the
/// rects, and therefore the quadrant the cursor is in.
///
/// Returns the row's "who's online" tooltip lines when the cursor is over its
/// status text (vanilla's `onlinePlayersTooltip` hover, `:356-361`) — `None`
/// otherwise. The one piece of this entry that *escapes* the row: the caller
/// draws the tooltip last, outside the band clip.
///
/// ## Two things that are not vanilla's, named rather than hidden
///
/// - **The MOTD wrap is greedy on whitespace**, where vanilla's `font.split` is a
///   full `StringSplitter` that also breaks inside an over-long word and carries
///   style across the break. A word wider than the column is therefore drawn past
///   the wrap width here instead of being cut; the column is 267 px, so it takes a
///   ~44-character unbroken run to notice.
/// - **The row's own background is the screen's**, not a per-row texture. Vanilla
///   blits `menu_list_background.png` tiled across the whole list band
///   (`AbstractSelectionList.java:226-238`) and draws *no* per-row fill for an
///   unselected row, so an unselected row here correctly paints nothing but its
///   content. The band texture itself is a loose `textures/gui/` PNG (the same
///   89-texture gap `resources.rs` documents) and is left to the flat [`BG`]
///   fill — which is what every other menu screen already draws.
fn draw_server_entry(
    b: &mut Quads<'_>,
    rows: &[MenuRow],
    i: usize,
    width: f32,
    height: f32,
    cursor: Option<(f32, f32)>,
) -> Option<Vec<String>> {
    let Some(row) = rows.get(i) else { return None };
    let Some(view) = row.entry.as_ref() else { return None };
    // `extractListItems` only draws the rows inside the band (`:346-352`); this is
    // that test, standing in for the scissor this pipeline has no equivalent of.
    // `row_rect` below now performs the same check on the way to its rect
    // (#402), so this one is a fast-out, not the only guard.
    if !server_row_visible(view.index, height, view.scroll) {
        return None;
    }
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return None;
    };

    // `extractItem`: the selected row gets a 1 px outline of `-1` when the list is
    // focused and `-8355712` when it is not, with the interior filled black —
    // drawn *under* the content (`:354-370`). This shell's list is focused
    // whenever the screen is up (there is nowhere else for the keyboard to be, and
    // the footer buttons are mouse-driven), so the outline is the focused one.
    if view.selected {
        b.rect(x, y, w, h, LABEL);
        b.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, SERVER_LIST_SELECTION_FILL);
    }

    let (cx, cy, cw, _) = server_row_content_rect(view.index, width, view.scroll);
    let (ix, iy, iw, ih) = server_entry_icon_rect(view.index, width, view.scroll);
    let text_x = cx + SERVER_ENTRY_ICON + SERVER_ENTRY_TEXT_GAP;

    // The favicon, or `FaviconTexture`'s fallback when the server sent none
    // (`:313,438-440`). The mosaic path is this shell's stand-in for a per-server
    // runtime texture — see the module docs.
    if let Some(icon) = row.favicon.as_ref() {
        b.mosaic(icon, ix, iy, iw);
    } else {
        b.sprite(SERVER_UNKNOWN_ICON, ix, iy, iw, ih, LABEL);
    }

    // The status column first, because the name's room depends on where it lands.
    let (icon_x, icon_y, icon_w, icon_h) = server_status_icon_rect(view.index, width, view.scroll);
    b.sprite(view.status_sprite, icon_x, icon_y, icon_w, icon_h, LABEL);
    let status_w = b.text_width(&view.status, 1.0);
    let status_x = icon_x - status_w - SERVER_ENTRY_SPACING;
    if !view.status.is_empty() {
        let colour = if view.status_is_error {
            SERVER_ENTRY_INCOMPATIBLE
        } else {
            SERVER_ENTRY_DIM
        };
        b.text(&view.status, status_x, cy + 1.0, 1.0, colour);
    }

    // `graphics.text(font, serverData.name, contentX + 32 + 3, contentY + 1, -1)`
    // (`:306`). Vanilla does not clip the name — it can and does run under the
    // status column — but this shell has no scissor, so it is clipped to the room
    // the status column leaves rather than drawn over it.
    let name = clip_measured(b, &row.label, (status_x - text_x).max(0.0));
    b.text(name, text_x, cy + 1.0, 1.0, LABEL);

    // Up to two MOTD lines at `contentY + 12 + 9 * i`, wrapped to
    // `contentWidth - 32 - 2` (`:307-311`).
    let motd_colour = if view.motd_is_error {
        SERVER_ENTRY_BAD
    } else {
        SERVER_ENTRY_DIM
    };
    let wrap_w = (cw - SERVER_ENTRY_MOTD_INSET).max(0.0);
    let lines = wrap_measured(b, &view.motd, wrap_w, SERVER_ENTRY_MOTD_LINES);
    if view.motd_spans.is_empty() {
        // No server styling (a synthetic MOTD, or a server that sent none):
        // the pre-existing flat draw, unchanged.
        for (line, text) in lines.iter().enumerate() {
            b.text(
                text,
                text_x,
                cy + SERVER_ENTRY_MOTD_Y + LINE_H * line as f32,
                1.0,
                motd_colour,
            );
        }
    } else {
        // `motd_colour` becomes the *base*: a run the server left uncoloured
        // still renders in vanilla's dim grey, and only the runs it coloured
        // differ. That is what makes this behaviour-preserving for the many
        // MOTDs that carry no colour at all.
        let base = [motd_colour[0], motd_colour[1], motd_colour[2]];
        for (line, runs) in restyle_wrapped(&view.motd_spans, &lines)
            .iter()
            .enumerate()
        {
            b.text_spans(
                runs,
                text_x,
                cy + SERVER_ENTRY_MOTD_Y + LINE_H * line as f32,
                1.0,
                base,
                motd_colour[3],
            );
        }
    }

    // The hover overlay (`:364-395`). All three sprites blit at the *same* 32×32
    // icon rect, and only the one whose quadrant holds the cursor is drawn
    // highlighted — so the discriminator is position, not which row is hovered.
    let Some((mx, my)) = cursor else { return None };
    if mx < x || mx >= x + w || my < y || my >= y + h {
        return None;
    }
    b.rect(ix, iy, iw, ih, SERVER_ICON_DARKEN);
    let (rx, ry) = (mx - ix, my - iy);
    let pick = |hit: bool, sprites: (&'static str, &'static str)| {
        if hit { sprites.1 } else { sprites.0 }
    };
    let mut blit = |id: &'static str| b.sprite(id, ix, iy, iw, ih, LABEL);
    blit(pick(
        widget::over_right_half(rx, ry, iw),
        SERVER_JOIN_SPRITES,
    ));
    if view.can_move_up {
        blit(pick(
            widget::over_top_left_quarter(rx, ry, iw),
            SERVER_MOVE_UP_SPRITES,
        ));
    }
    if view.can_move_down {
        blit(pick(
            widget::over_bottom_left_quarter(rx, ry, iw),
            SERVER_MOVE_DOWN_SPRITES,
        ));
    }

    // Vanilla's "who's online" tooltip. It is not row-wide: it fires only when
    // the cursor is over the status *text* — the player count, or an
    // incompatible server's version string —
    // `mouseX >= statusX && mouseX <= statusX + statusWidth && mouseY >=
    // getContentY() && mouseY <= getContentY() - 1 + 9`
    // (`ServerSelectionList.java:356-361`). The ping-latency tooltip vanilla
    // checks first (`:358-362`) is deliberately absent — the "who's online"
    // half of the screen is the half this shell has the model for, and the icon
    // and text rects are disjoint either way.
    if !view.online_players.is_empty()
        && mx >= status_x
        && mx <= status_x + status_w
        && my >= cy
        && my <= cy + 8.0
    {
        return Some(view.online_players.clone());
    }
    None
}

/// Draws the multiplayer list's "who's online" tooltip — vanilla's
/// `DefaultTooltipPositioner`-positioned `TooltipRenderUtil` box
/// (`DefaultTooltipPositioner.java`, `TooltipRenderUtil.java`), which this
/// pipeline has no sprite path for, so it draws the two sprites' *visible*
/// pixels as flat quads instead.
///
/// ## Why these exact rects
///
/// `TooltipRenderUtil.renderTooltipInternal` positions the **content** box
/// (text only, `w×h`), then blits `tooltip/background.png` inset by
/// [`TOOLTIP_PAD`] + its 9 px transparent border, and `tooltip/frame.png` with
/// its 10 px border — i.e. the fill at `[x-3, y-3, w+6, h+6]`, the frame at
/// `[x-2, y-2, w+5, h+5]`. The background sprite was decoded out of `client.jar`
/// (a 1-bit indexed, 9 px-border nine-slice): its whole opaque centre is one
/// colour, [`TOOLTIP_BG`]. The frame sprite is a 10 px-border nine-slice whose
/// only opaque pixels are 1 px bars along the four border rows/columns — top and
/// left at [`TOOLTIP_FRAME_TOP`], bottom and right at
/// [`TOOLTIP_FRAME_BOTTOM`], the two ends of the gradient its vertical bars
/// carry. So the outline below is not a stylised box; it is the frame sprite's
/// nine-slice geometry, corners open.
///
/// Positioning is `DefaultTooltipPositioner` (`:13-29`) exactly: content top-left
/// at the cursor + ([`TOOLTIP_MOUSE_OFFSET`], -[`TOOLTIP_MOUSE_OFFSET`]),
/// flipping left of the cursor when that runs past the right edge, and clamped
/// to the bottom. Text is drawn white with vanilla's drop shadow, which this
/// pipeline's two fonts already draw.
fn draw_tooltip(b: &mut Quads<'_>, lines: &[String], at: (f32, f32), width: f32, height: f32) {
    if lines.is_empty() {
        return;
    }
    let w = lines
        .iter()
        .map(|l| b.text_width(l, 1.0))
        .fold(0.0_f32, f32::max);
    // `ClientTextTooltip.getHeight` (`:20-22`): `(n == 1 ? -2 : 0) + 10n`.
    let h = if lines.len() == 1 {
        8.0
    } else {
        TOOLTIP_LINE_H * lines.len() as f32
    };
    let mut rx = at.0 + TOOLTIP_MOUSE_OFFSET;
    let mut ry = at.1 - TOOLTIP_MOUSE_OFFSET;
    if rx + w > width {
        rx = (rx - 2.0 * TOOLTIP_MOUSE_OFFSET - w).max(4.0);
    }
    let padded = h + TOOLTIP_PAD;
    if ry + padded > height {
        ry = height - padded;
    }
    // The fill.
    b.rect(
        rx - TOOLTIP_PAD,
        ry - TOOLTIP_PAD,
        w + 2.0 * TOOLTIP_PAD,
        h + 2.0 * TOOLTIP_PAD,
        TOOLTIP_BG,
    );
    // The frame's four 1 px bars, at the nine-slice geometry above.
    b.rect(rx - 2.0, ry - TOOLTIP_PAD, w + 4.0, 1.0, TOOLTIP_FRAME_TOP);
    b.rect(rx - TOOLTIP_PAD, ry - 2.0, 1.0, h + 4.0, TOOLTIP_FRAME_TOP);
    b.rect(rx - 2.0, ry + h + 2.0, w + 4.0, 1.0, TOOLTIP_FRAME_BOTTOM);
    b.rect(rx + w + 2.0, ry - 2.0, 1.0, h + 4.0, TOOLTIP_FRAME_BOTTOM);
    // `ClientTextTooltip.renderText` (`:33-36`): first line at `y`, then the
    // +2 interline gap brings each later line to `y + 12 + 10*(i-1)`.
    for (i, line) in lines.iter().enumerate() {
        let ly = if i == 0 {
            ry
        } else {
            ry + TOOLTIP_MOUSE_OFFSET + TOOLTIP_LINE_H * (i - 1) as f32
        };
        b.text(line, rx, ly, 1.0, LABEL);
    }
}

/// Greedy whitespace wrap of `s` to at most `max_lines` lines of `max_px`, in
/// whatever font `b` draws with — this shell's stand-in for `Font.split`.
///
/// Explicit `\n`s break a line too, because a MOTD is a chat component that has
/// already been flattened to text by `lodestone_net` and may carry them; vanilla
/// splits on both.
///
/// A single word wider than `max_px` is *not* broken (see [`draw_server_entry`]'s
/// note), and lines past `max_lines` are dropped rather than truncated with an
/// ellipsis, because the font has no ellipsis glyph.
pub(super) fn wrap_measured(b: &Quads<'_>, s: &str, max_px: f32, max_lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for paragraph in s.split('\n') {
        // Per paragraph, not per call: without this the first word after a `\n`
        // would be appended to the previous paragraph's last line whenever it
        // happened to fit, which is the one thing splitting on `\n` exists to stop.
        let mut open_line = false;
        for word in paragraph.split_whitespace() {
            if out.len() >= max_lines {
                return out;
            }
            let fits = open_line
                && out.last().is_some_and(|line| {
                    b.text_width(&format!("{line} {word}"), 1.0) <= max_px
                });
            if fits {
                if let Some(line) = out.last_mut() {
                    line.push(' ');
                    line.push_str(word);
                }
            } else {
                // A word that does not fit starts a line rather than overflowing
                // the one before it.
                out.push(word.to_string());
                open_line = true;
            }
        }
        // A blank line in the MOTD is a line: vanilla's split keeps it, and losing
        // it would pull a two-line MOTD's second line up into the first's place.
        if out.len() < max_lines && paragraph.trim().is_empty() {
            out.push(String::new());
        }
    }
    out.truncate(max_lines);
    out
}

/// Re-attach `spans`' per-character styles to the plain lines [`wrap_measured`]
/// produced.
///
/// ## Why this rather than a span-aware wrapper
///
/// The obvious move is a `wrap_measured` that understands spans. That means a
/// second copy of the word-wrap algorithm, and the two would drift — vanilla's
/// MOTD wrap has several non-obvious rules (per-paragraph line state, a blank
/// line is a line, an over-wide word starts a line rather than overflowing) that
/// exist because each was a bug once. One wrapper, one set of rules.
///
/// This works because a wrapped line's characters are a **subsequence** of the
/// source, in order: the wrapper only splits on whitespace and rejoins with a
/// single space. So walking both in lockstep and skipping the whitespace the
/// wrapper collapsed re-attaches each character to the style it came with.
/// Adjacent equal styles are merged so the draw sees runs, not one span per
/// character.
fn restyle_wrapped(spans: &[TextSpan], lines: &[String]) -> Vec<Vec<TextSpan>> {
    let flat: Vec<(char, TextStyle)> = spans
        .iter()
        .flat_map(|s| s.text.chars().map(move |c| (c, s.style)))
        .collect();
    let mut cursor = 0usize;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let mut runs: Vec<TextSpan> = Vec::new();
        for ch in line.chars() {
            // Skip forward over the whitespace `wrap_measured` dropped. A probe
            // that runs off the end leaves the style unspecified rather than
            // mis-assigning one, so a desync degrades to `base` instead of
            // smearing a wrong colour across the rest of the MOTD.
            let mut probe = cursor;
            while probe < flat.len() && flat[probe].0 != ch {
                probe += 1;
            }
            let style = if probe < flat.len() {
                cursor = probe + 1;
                flat[probe].1
            } else {
                TextStyle::default()
            };
            match runs.last_mut() {
                Some(last) if last.style == style => last.text.push(ch),
                _ => runs.push(TextSpan {
                    text: ch.to_string(),
                    style,
                }),
            }
        }
        out.push(runs);
    }
    out
}

/// [`wrap_measured`], except that a word wider than the column is **broken**
/// instead of overflowing it. Used by [`MenuNotice`].
///
/// ## Why this is a second function rather than a flag on the first
///
/// [`wrap_measured`] deliberately does *not* break inside a word, because
/// `ServerSelectionList` wraps a MOTD with `Font.split` and the difference only
/// shows on a ~44-character unbroken run — a documented simplification of the
/// multiplayer screen, and not one worth changing under it.
///
/// For a notice the simplification is a **bug**, not a rounding error, and it is
/// the bug that was reported: an `AuthError` can carry a snippet of a raw HTTP
/// response body, and JSON has no whitespace in it at all, so a whitespace-only
/// wrap emits one enormous line and the greedy fallback ("a word that does not
/// fit starts a line") does not save it. This is also *closer* to vanilla than
/// its sibling — `StringSplitter` breaks mid-word too — so the two are not a
/// fidelity choice, they are two different requirements.
///
/// The single-glyph guard is load-bearing: at a column narrower than one
/// character [`clip_measured`] returns `""`, and pushing an empty line forever is
/// how this would hang instead of drawing.
pub(super) fn wrap_bounded(b: &Quads<'_>, s: &str, max_px: f32, max_lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if max_lines == 0 {
        return out;
    }
    for paragraph in s.split('\n') {
        // Per paragraph, for `wrap_measured`'s reason: an explicit newline must
        // not be swallowed by the next word happening to fit.
        let mut open_line = false;
        for word in paragraph.split_whitespace() {
            if out.len() >= max_lines {
                return out;
            }
            // Extend the open line if the whole word still fits on it. The fit is
            // computed into a `bool` first, exactly as `wrap_measured` does it:
            // holding the `out.last()` borrow across the `out.last_mut()` that
            // follows would not compile.
            let fits = open_line
                && out
                    .last()
                    .is_some_and(|line| b.text_width(&format!("{line} {word}"), 1.0) <= max_px);
            if fits {
                if let Some(line) = out.last_mut() {
                    line.push(' ');
                    line.push_str(word);
                }
                continue;
            }
            // Otherwise the word starts a line — and keeps starting lines until
            // what is left of it fits on one.
            let mut rest = word;
            loop {
                if out.len() >= max_lines {
                    return out;
                }
                let head = clip_measured(b, rest, max_px);
                let head = if head.is_empty() {
                    match rest.char_indices().nth(1) {
                        Some((i, _)) => &rest[..i],
                        None => rest,
                    }
                } else {
                    head
                };
                out.push(head.to_string());
                rest = &rest[head.len()..];
                if rest.is_empty() {
                    break;
                }
            }
            open_line = true;
        }
        if out.len() < max_lines && paragraph.trim().is_empty() {
            out.push(String::new());
        }
    }
    out.truncate(max_lines);
    out
}

/// Draws one account-list row: the cursor outline, the head icon, the username,
/// the "Selected" marker and the row's small detail line.
///
/// [`draw_server_entry`]'s shape at [`ACCOUNTS_ITEM_H`] pitch, minus the favicon
/// quadrants (there is nothing to reorder or join on this screen) and minus the
/// status sprite. Like that function it **decides nothing**: which row is
/// outlined and what the three text columns say are resolved into the row and its
/// [`AccountEntryView`] by [`accounts_idle_frame`]; what it owns is the
/// canvas-dependent part, which is the rects.
///
/// Every text column is measured and clipped in the font `b` draws with, and the
/// name is clipped to the room the marker leaves rather than being drawn under it
/// — this pipeline has no scissor, so an over-long username would otherwise
/// overprint the marker instead of being cut by it.
fn draw_account_entry(b: &mut Quads<'_>, rows: &[MenuRow], i: usize, width: f32, height: f32) {
    let Some(row) = rows.get(i) else { return };
    let Some(view) = row.account.as_ref() else {
        return;
    };
    if !accounts_row_visible(view.index, height, view.scroll) {
        return;
    }
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };

    // `AbstractSelectionList.extractItem`'s selection pass: a 1 px outline with
    // the interior filled black, drawn *under* the content (`:354-370`). The
    // outline is the focused variant because this screen's list is focused
    // whenever it is up — the buttons are a separate cursor (see
    // `accounts_idle_frame`).
    if view.selected {
        b.rect(x, y, w, h, LABEL);
        b.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, ACCOUNTS_SELECTION_FILL);
    }

    let (cx, cy, cw, _) = accounts_row_content_rect(view.index, width, view.scroll);
    let text_x = cx + ACCOUNTS_HEAD_ICON + ACCOUNTS_TEXT_GAP;

    // The head, through the same `FaviconMosaic` path a server's favicon takes —
    // see `MenuRow::head` on why a head is not a second kind of drawable.
    if let Some(head) = row.head.as_ref() {
        b.mosaic(head, cx, cy, ACCOUNTS_HEAD_ICON);
    }

    // The marker first, because the name's room depends on where it lands.
    let marker_w = b.text_width(&row.trailing, 1.0);
    let marker_x = cx + cw - ACCOUNTS_SPACING - marker_w;
    if !row.trailing.is_empty() {
        b.text(&row.trailing, marker_x, cy + 1.0, 1.0, LABEL);
    }
    let name = clip_measured(b, &row.label, (marker_x - ACCOUNTS_SPACING - text_x).max(0.0));
    b.text(name, text_x, cy + 1.0, 1.0, LABEL);

    let detail_room = (cx + cw - ACCOUNTS_SPACING - text_x).max(0.0);
    let detail = clip_measured(b, &row.detail, detail_room);
    b.text(detail, text_x, cy + ACCOUNTS_DETAIL_Y, 1.0, ACCOUNTS_DIM);
}

/// Draws one world-list row — `WorldSelectionList.WorldListEntry.extractContent`
/// (`WorldSelectionList.java:555-570`).
///
/// The selection outline, then three text lines at
/// [`WORLD_LIST_LINE_DY`]'s offsets, all measured from the row's **content** rect
/// (the 36 px entry inset by `CONTENT_PADDING`) and inset again by
/// [`WORLD_LIST_TEXT_DX`] for the icon column.
///
/// **The icon column is reserved and left empty**, and that is deliberate rather
/// than unfinished: vanilla blits a 32×32 `FaviconTexture.forWorld` there, backed
/// by the `icon.png` the client writes on quit. This client writes none (see
/// `crate::saves::WorldSummary`'s doc on the fields it deliberately does not
/// port), so there is nothing to blit — and the column still has to exist,
/// because all three text lines' x is measured from its far edge. Drawing a
/// placeholder square would be inventing a texture vanilla does not have for a
/// world with no icon.
///
/// Every string is clipped to [`world_list_text_width`] with the same
/// [`clip_measured`] the account row uses, which is `StringWidget.setMaxWidth`
/// (`:418`, `:436`, `:441`) — vanilla additionally attaches a tooltip when it
/// clips, still unported for #393's reason (nothing tracks hover dwell time).
fn draw_world_entry(b: &mut Quads<'_>, rows: &[MenuRow], i: usize, width: f32, height: f32) {
    let Some(row) = rows.get(i) else { return };
    let Some(view) = row.world.as_ref() else {
        return;
    };
    // The gate is asked here as well as inside `row_rect` for `draw_server_entry`'s
    // reason: this is the draw's own statement of what is on screen, and the two
    // must agree by both reading the same predicate rather than by one trusting
    // the other's `None`.
    // The re-clamped offset — `world_list_scroll_for`, the same function
    // `row_rect` reads, so the draw and the hit-test cannot disagree about where a
    // row is.
    let scroll = world_list_scroll_for(rows, height);
    if !world_list_row_visible(view.index, height, scroll) {
        return;
    }
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };

    if view.selected {
        b.rect(x, y, w, h, LABEL);
        b.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, WORLD_LIST_SELECTION_FILL);
    }

    let (cx, cy, ..) = world_list_row_content_rect(view.index, width, scroll);
    let text_x = cx + WORLD_LIST_TEXT_DX;
    let room = world_list_text_width();
    // Name, folder + last played, game mode + version — in `MenuRow`'s own
    // `label`/`detail`/`trailing`, read off the row rather than duplicated into
    // `WorldEntryView`. Only the first is white; see `WORLD_LIST_DIM`.
    for (line, (text, colour)) in [
        (row.label.as_str(), LABEL),
        (row.detail.as_str(), WORLD_LIST_DIM),
        (row.trailing.as_str(), WORLD_LIST_DIM),
    ]
    .into_iter()
    .enumerate()
    {
        if text.is_empty() {
            continue;
        }
        b.text(
            clip_measured(b, text, room),
            text_x,
            cy + WORLD_LIST_LINE_DY[line],
            1.0,
            colour,
        );
    }
}

/// Draws one resource-pack row — `TransferableSelectionList.PackEntry.extractContent`
/// (`TransferableSelectionList.java:136-219`).
///
/// The selection outline, the 32×32 `pack.png` thumbnail, the pack name, up to two
/// description lines under it, and — while the row is the list's selection or under
/// the cursor — vanilla's `transferable_list/select`/`unselect` overlay on the icon.
///
/// Like [`draw_world_entry`] it **decides nothing**: the name is the row's `label`,
/// the description its `detail`, the thumbnail its `favicon`, and which overlay
/// applies is resolved into [`PackEntryView`] by [`super::packs::frame`]. What it
/// owns is the canvas-dependent part, which is the rects.
///
/// ## Three named departures from the jar
///
/// - **The overlay is an indicator, not a hit zone.** Vanilla's icon carries four
///   click quadrants (select/unselect on the icon or its left half, move up/down on
///   the two right quarters); here the *whole row* transfers the pack and the two
///   reorder buttons are separate widgets to its right — this client's shape, for
///   the reason [`super::packs`]'s module doc records. So the `_highlighted` sprite
///   variant still tracks the cursor being over the icon, exactly as vanilla's
///   `mouseOverIcon` does, but the plain variant is not a "click elsewhere" hint:
///   clicking anywhere on the row does the same thing.
/// - **Unselect uses the whole icon rather than its left half.** `mouseOverLeftHalf`
///   exists in vanilla because the right half holds the move quadrants. Nothing is
///   drawn there here, so splitting the icon would leave a dead half.
/// - **No incompatible marking.** Vanilla fills the content box dark red
///   (`-8978432`, `:139-144`) and swaps the name for `pack.incompatible` when
///   `PackCompatibility` rejects the pack's format. Still deliberately out of scope
///   for the reason `packs`'s module doc gives: nothing in this client declares a
///   *host* `pack_format` to compare against, and
///   [`crate::resources::DiscoveredPack`] drops `pack.mcmeta`'s
///   `supported_formats` range, so a guessed host number would paint a red bar over
///   packs that are in fact fine. Painting nothing is the honest reduction; a
///   wrong warning is not.
fn draw_pack_entry(
    b: &mut Quads<'_>,
    rows: &[MenuRow],
    i: usize,
    width: f32,
    height: f32,
    selected: bool,
    cursor: Option<(f32, f32)>,
) {
    let Some(row) = rows.get(i) else { return };
    let Some(view) = row.pack.as_ref() else {
        return;
    };
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };

    // `AbstractSelectionList.extractItem`'s selection pass: a 1 px outline with the
    // interior filled black, drawn *under* the content (`:354-370`). Focused
    // variant, for `draw_server_entry`'s reason — this screen's list is focused
    // whenever the cursor is on one of its rows.
    if selected {
        b.rect(x, y, w, h, LABEL);
        b.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, PACK_SELECTION_FILL);
    }

    // The content box: `getContentX()`/`getContentY()`, the entry inset by
    // `CONTENT_PADDING` (`AbstractSelectionList.java:477-495`).
    let (cx, cy) = (x + PACK_ROW_PAD, y + PACK_ROW_PAD);

    // `graphics.blit(…, this.pack.getIconTexture(), getContentX(), getContentY(), …,
    // 32, 32, …)` (`:146`). The mosaic path is this shell's stand-in for a per-pack
    // runtime texture, the same one a server favicon and an account head take; a
    // pack that ships no readable `pack.png` gets vanilla's own `DEFAULT_ICON`.
    match row.favicon.as_ref() {
        Some(icon) => b.mosaic(icon, cx, cy, PACK_ICON),
        None => b.sprite(PACK_UNKNOWN_ICON, cx, cy, PACK_ICON, PACK_ICON, LABEL),
    }

    // `if (this.showHoverOverlay() && (hovered || getSelected() == this && isFocused()))`
    // (`:155`) — so the overlay follows the *selection* as well as the mouse, which
    // is what makes it visible under keyboard navigation. A pack that can neither be
    // selected nor unselected (the built-in one: `isFixedPosition() && isRequired()`)
    // draws none at all, exactly as vanilla's does not.
    let in_rect = |(mx, my): (f32, f32), rx: f32, ry: f32, rw: f32, rh: f32| {
        mx >= rx && mx < rx + rw && my >= ry && my < ry + rh
    };
    let hovered = cursor.is_some_and(|at| in_rect(at, x, y, w, h));
    let sprites = if view.can_select {
        Some(PACK_SELECT_SPRITES)
    } else if view.can_unselect {
        Some(PACK_UNSELECT_SPRITES)
    } else {
        None
    };
    if let Some(sprites) = sprites {
        if selected || hovered {
            b.rect(cx, cy, PACK_ICON, PACK_ICON, PACK_ICON_DARKEN);
            let over_icon = cursor.is_some_and(|at| in_rect(at, cx, cy, PACK_ICON, PACK_ICON));
            let id = if over_icon { sprites.1 } else { sprites.0 };
            b.sprite(id, cx, cy, PACK_ICON, PACK_ICON, LABEL);
        }
    }

    // `nameWidget` at `getContentX() + 32 + 2, getContentY() + 1`, then
    // `descriptionWidget` at `+ 12` (`:213-218`), both `setMaxWidth(157)`. Vanilla's
    // `StringWidget`/`MultiLineTextWidget` clip and wrap to that width; here that is
    // `clip_measured` and `wrap_measured`, measured in the font this `Quads` will
    // actually draw with.
    let tx = cx + PACK_TEXT_DX;
    b.text(
        clip_measured(b, &row.label, PACK_TEXT_MAX_W),
        tx,
        cy + PACK_NAME_DY,
        1.0,
        LABEL,
    );
    let lines = wrap_measured(b, &row.detail, PACK_TEXT_MAX_W, PACK_DESC_ROWS);
    for (line, text) in lines.iter().enumerate() {
        b.text(
            text,
            tx,
            cy + PACK_DESC_DY + LINE_H * line as f32,
            1.0,
            PACK_ENTRY_DIM,
        );
    }
}

/// Draws one vanilla widget: its `widget/button*` nine-slice background, then
/// either its centred label or its centred 15×15 icon sprite.
///
/// **This is [`widget::Widget`]'s consumer.** Nothing about which sprite or which
/// label colour a state produces is decided here any more — a [`Widget`] is built
/// from the row's own `enabled`/`selected` and then *asked*
/// ([`Widget::background_sprite`], [`Widget::message_colour`]), so the title
/// screen, the pause menu, the death screen and the account screen's action row
/// share one copy of vanilla's rules instead of a three-way `if` per screen. That
/// is the whole point of #393: the fourth screen must not write the blit a fourth
/// time.
///
/// The rect still comes from [`row_rect`] rather than from the widget, and
/// deliberately: that function is also `app.rs`'s hit-test, so it stays the single
/// definition of where a row is until #394 gives the layout containers somewhere
/// to write positions *to*.
///
/// Mirrors `AbstractButton.extractDefaultSprite` +
/// `Button.Plain.extractContents` (`AbstractButton.java:43-53`,
/// `Button.java:128-132`) and, for icons,
/// `SpriteIconButton.CenteredIcon.extractContents`
/// (`SpriteIconButton.java:236-244`).
/// **`server_scroll_list` is gone.** It rebuilt the multiplayer list's geometry per
/// frame for the scrollbar, and it was the by-name call that made this file's
/// scrollbar the *multiplayer* list's rather than the active screen's. Its job now
/// belongs to `MenuNav::active_list`, which every screen answers, plus
/// `widget::ListSpec::model` — one declaration, and the draw asks the frame instead
/// of naming a screen. See `render::accounts_list_spec` for the second client.
/// Draw a [`widget::ScrollList`]'s scrollbar — `extractScrollbar`
/// (`AbstractScrollArea.java:110-137`).
///
/// Track then thumb, both from the list's own [`widget::ScrollList::scrollbar_rects`]
/// so the bar that draws and the bar [`widget::ScrollList::is_over_scrollbar`]
/// hit-tests are the same geometry. Nothing is drawn when the list does not
/// scroll, which is vanilla's `if (this.scrollable())` gate (`:126`) — and is why
/// a list that fits shows no bar at all rather than a full-height stub.
///
/// **The jar-less fallback is not a citation.** 26.2 draws `widget/scroller` and
/// `widget/scroller_background` and nothing else, so there is no vanilla colour
/// for a run with no atlas; rather than invent one, the fallback reuses this
/// shell's existing [`ROW_OFF`]/[`LABEL`] palette. Do not turn those into
/// "vanilla's scrollbar colours" — they are ours.
fn draw_scrollbar(b: &mut Quads<'_>, list: &widget::ScrollList, row_right: f32) {
    let Some((track, thumb)) = list.scrollbar_rects(row_right) else {
        return;
    };
    let (tx, ty, tw, th) = track;
    let (hx, hy, hw, hh) = thumb;
    if b.has_sprite(widget::SCROLLER_BACKGROUND_SPRITE) && b.has_sprite(widget::SCROLLER_SPRITE) {
        b.sprite(widget::SCROLLER_BACKGROUND_SPRITE, tx, ty, tw, th, LABEL);
        b.sprite(widget::SCROLLER_SPRITE, hx, hy, hw, hh, LABEL);
    } else {
        b.rect(tx, ty, tw, th, ROW_OFF);
        b.rect(hx, hy, hw, hh, LABEL);
    }
}

fn draw_widget(
    b: &mut Quads<'_>,
    rows: &[MenuRow],
    i: usize,
    width: f32,
    height: f32,
    selected: bool,
    hovered: bool,
) {
    let Some(row) = rows.get(i) else { return };
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };
    // One widget, carrying this row's state. `focused` takes `selected`; on the
    // title screen, the pause menu, the death screen and the account screen
    // `hovered` is always `false`, because those still have a *single* row cursor
    // that both the keyboard and `MenuNav::hover` move — there is no second fact
    // to record. #395 split the two flags on `Widget` for the screens that do have
    // real focus (`Screen::ServerEdit`'s fields), and #397's `Screen::WorldSelect`
    // is the first screen to carry *both* through a frame, via
    // `MenuFrame::hovered`. Vanilla's sprite argument is `isHoveredOrFocused()`,
    // and that `||` lives in `Widget::is_hovered_or_focused`; do not re-derive it
    // in this function.
    //
    // Built per frame, so the message is copied per frame. That is the same cost
    // the row itself already pays — `frame_for` and `pause_frame` both rebuild
    // every `MenuRow` with a fresh `label.to_string()` every frame — and a menu
    // screen draws nine of these with no world behind it, so it is not worth a
    // lifetime parameter on `Widget` to avoid.
    // A settings slider carries `AbstractSliderButton`'s track instead of
    // `AbstractButton`'s three `widget/button*` states (issue #55). Which sprite
    // set a row gets is still the *widget's* decision, not this function's — the
    // only thing decided here is which kind of widget the row is.
    let mut widget = if row.slider {
        Widget::slider(x, y, w, h, row.label.as_str())
    } else {
        Widget::button(x, y, w, h, row.label.as_str())
    };
    widget.active = row.enabled;
    widget.focused = selected;
    widget.hovered = hovered;
    widget.icon = row.icon;
    // `AbstractWidget.extractRenderState` wraps everything in `if (this.visible)`
    // (`AbstractWidget.java:56-62`). No row sets this yet; the guard is here so
    // that the day one does, it does not have to be remembered.
    if !widget.visible {
        return;
    }

    // `WidgetSprites::get(active, hoveredOrFocused)` (`WidgetSprites.java:18-24`)
    // with `AbstractButton`'s three-argument sprite set: disabled wins over
    // hovered, which is why a greyed-out button under the cursor still looks
    // greyed out. The rule lives in `menu::widget`; this only asks.
    //
    // A slider asks a *different* question — `AbstractSliderButton.getSprite()`
    // passes `isActive()` and `isFocused()` alone, so hovering one does not
    // highlight it (`AbstractSliderButton.java:36-38`). Both predicates live on
    // `Widget`; neither is re-derived here.
    let background = if row.slider {
        widget.slider_background_sprite()
    } else {
        widget.background_sprite()
    };
    match background {
        Some(sprite) if b.has_sprite(sprite) => b.sprite(sprite, x, y, w, h, LABEL),
        _ => {
            // Jar-less fallback: the flat fills the menu has always used, so the
            // layout is still legible and still testable without a pack. The
            // predicate is the widget's `||`, not `focused` alone, so the fallback
            // cannot disagree with the sprite path above about which button is
            // lit — identical for every screen with a row cursor, since `hovered`
            // is false there.
            let fill = if !widget.active {
                ROW_OFF
            } else if widget.is_hovered_or_focused() {
                ROW_SEL
            } else {
                ROW_BG
            };
            b.rect(x, y, w, h, fill);
            if widget.is_hovered_or_focused() {
                b.outline(x, y, w, h, 1.0, FG);
            }
        }
    }

    // `AbstractSliderButton.extractWidgetRenderState` blits the handle right
    // after the track and before the label (`AbstractSliderButton.java:67-78`):
    // `getX() + (int)(this.value * (this.width - 8))`, width 8, full row
    // height. `row.slider_value` is `None` for a slider this client holds no
    // value for at all (see its doc) — that slider keeps drawing bare, exactly
    // as it did before this existed, rather than getting a fabricated handle.
    // Gated on `has_sprite` like the track above: no atlas, no handle, same
    // jar-less fallback discipline.
    if row.slider {
        if let Some(fraction) = row.slider_value {
            let handle_sprite = widget.slider_handle_sprite();
            if b.has_sprite(handle_sprite) {
                let hx = x + (fraction.clamp(0.0, 1.0) * (w - SLIDER_HANDLE_WIDTH)).floor();
                b.sprite(handle_sprite, hx, y, SLIDER_HANDLE_WIDTH, h, LABEL);
            }
        }
    }

    if let Some(icon) = widget.icon {
        // `spriteOffset` is zero at every call site, so this is a plain centre.
        let (ix, iy) = widget.icon_rect(ICON_SPRITE);
        b.sprite(icon, ix, iy, ICON_SPRITE, ICON_SPRITE, ICON_TINT);
        return;
    }

    // A **triangle** drawn centred instead of the label, for a button whose whole
    // meaning is a direction: the Resource Packs screen's two reorder buttons
    // (issue #415). Geometry rather than a glyph because the fallback font is
    // upper-case 5×7 with no arrow in it — see `MenuRow::arrow`.
    //
    // This replaces the pack-*row* draw that used to live here, gated on
    // `MenuRow::favicon`: a pack row is a selection-list entry, not a button with
    // an icon in it, so it is `draw_pack_entry`'s now and never reaches this
    // function. Drawing it here was the reported bug — a pack with no `pack.png`
    // has no `favicon`, so the built-in row (and any hand-made pack) missed the
    // branch entirely and came out as a plain centred-label button.
    if let Some(arrow) = row.arrow {
        draw_arrow(b, arrow, x, y, w, h, widget.message_colour());
        return;
    }

    let colour = widget.message_colour();
    // `extractScrollingStringOverContents(output, message, 2)` →
    // `acceptScrollingWithDefaultCenter(msg, x+2, x+w-2, y, y+h)`
    // (`AbstractButton.java:39-41`, `AbstractWidget.java:92-98`), whose centre
    // is `(left + right) / 2` and whose top is
    // `(top + bottom - lineHeight) / 2 + 1` (`ActiveTextCollector.java:59,73`).
    let (left, right) = widget.content_span();
    let tw = b.text_width(&widget.message, 1.0);
    let label = if tw > right - left {
        // Vanilla scrolls an over-long label; we clip, which is the same static
        // frame a scroll happens to be showing at t=0.
        clip_measured(b, &widget.message, right - left)
    } else {
        widget.message.as_str()
    };
    let tw = b.text_width(label, 1.0);
    let tx = ((left + right) * 0.5 - tw * 0.5).floor();
    let ty = widget.label_top(LINE_H);
    b.text(label, tx, ty, 1.0, colour);
}

/// Width of a [`MenuRow::arrow`] triangle, in logical pixels. Odd, so the apex
/// row is a single centred pixel column and the whole shape is symmetric about
/// the widget's centre.
const ARROW_W: f32 = 7.0;
/// Height of a [`MenuRow::arrow`] triangle: four 1 px rows of 1, 3, 5 and 7 —
/// `(ARROW_W + 1) / 2`, which is what makes the two edges a clean 45°.
const ARROW_H: f32 = (ARROW_W + 1.0) * 0.5;

/// A solid triangle centred in `(x, y, w, h)`, apex up or down.
///
/// Four stacked 1 px rows rather than a real triangle mesh: [`Quads`] emits
/// axis-aligned quads only (its clip is a rect intersection, and its text path
/// reconstructs glyphs from their bounding boxes), so a diagonal edge has to be a
/// staircase either way — which is also what vanilla's own 32×32
/// `transferable_list/move_up` sprite is, at a larger size.
fn draw_arrow(b: &mut Quads<'_>, arrow: Arrow, x: f32, y: f32, w: f32, h: f32, colour: [f32; 4]) {
    let ax = (x + (w - ARROW_W) * 0.5).floor();
    let ay = (y + (h - ARROW_H) * 0.5).floor();
    let rows = ARROW_H as usize;
    for row in 0..rows {
        // Widths 1, 3, 5, 7 measured from the apex, which is the top row for `Up`
        // and the bottom row for `Down`.
        let step = match arrow {
            Arrow::Up => row,
            Arrow::Down => rows - 1 - row,
        };
        let run = 1.0 + 2.0 * step as f32;
        b.rect(
            ax + (ARROW_W - run) * 0.5,
            ay + row as f32,
            run,
            1.0,
            colour,
        );
    }
}

/// Draws one [`EditBox`]: its `widget/text_field` background, the selection
/// block, the text either side of the caret, and the caret.
///
/// **This is `EditBox`'s draw consumer, and it decides nothing.** Every offset
/// comes from [`EditBox::draw_state`], every colour from
/// [`EditBox::text_colour`], and the sprite id from
/// [`EditBox::background_sprite`] — which is `SPRITES.get(isActive(),
/// isFocused())`, *not* the button's `isHoveredOrFocused()`, so hovering a field
/// deliberately does not highlight it. Mirrors
/// `EditBox.extractWidgetRenderState` (`EditBox.java:404-473`).
///
/// ## The reposition, and why it is on a clone
///
/// The widget lives in [`super::nav::EditForm`] and outlives the frame;
/// `frame_for` takes `&MenuNav`, so the row carries a *copy* and this function
/// moves the copy into `(x, y, w, h)` before reading it. That is
/// `OptionsSubScreen.init`'s build → reposition order (`:28-34`) rather than
/// `PauseScreen`'s build → arrange, which is the switch #394 predicted would
/// happen "once a widget holds state". The seeded geometry in `EditForm` is what
/// the *input* side measures against; see [`field_row_rects`].
///
/// ## Two deliberate departures from the jar
///
/// - **The caret and the selection used to be 14 px tall, not 11.** This
///   bullet used to justify that against [`TEXT_SCALE`] `2.0` — but a player
///   report (2026-08-04) caught that this function was the *only* thing on a
///   vanilla-positioned screen still drawing at that scale, against
///   `9`-tall vanilla glyphs (`Font.java:33`) in a 20 px box: a 0.70 fill
///   ratio where every sibling widget (`draw_widget`'s buttons, at `1.0`)
///   sits at 0.45. Fixed by [`EDIT_TEXT_SCALE`] `1.0`, so a glyph is
///   `GLYPH_H * 1 = 7` tall — see that constant's own doc. The *horizontal*
///   arithmetic in the widget (`edit_box::MENU_TEXT_ADVANCE`) has to move in
///   lockstep or the caret advance disagrees with the glyphs it steps over;
///   that half is `edit_box.rs`'s, not this function's.
/// - **The append caret is a bar, not an `_` glyph.** `extractAppendCursor`
///   draws the underscore character (`TextCursorUtils.java:16-18`), and the
///   jar-less fallback font here has no guaranteed `_`. Drawing a baseline bar
///   keeps the insert/append distinction visible without depending on a glyph
///   that may not exist, and the distinction itself
///   ([`edit_box::EditBoxDraw::insert_cursor`]) is asserted in `edit_box`'s own
///   tests.
fn draw_edit_box(b: &mut Quads<'_>, edit: &EditBox, x: f32, y: f32, w: f32, h: f32) {
    let mut edit = edit.clone();
    edit.widget.x = x;
    edit.widget.y = y;
    edit.widget.width = w;
    edit.widget.height = h;
    // `AbstractWidget.extractRenderState`'s `if (this.visible)`
    // (`AbstractWidget.java:56-62`).
    if !edit.widget.visible {
        return;
    }

    match edit.background_sprite() {
        Some(sprite) if b.has_sprite(sprite) => b.sprite(sprite, x, y, w, h, LABEL),
        // Jar-less fallback: the flat field fill the form has always used, plus
        // a border when focused so the focused field is still identifiable in a
        // headless screenshot.
        _ => {
            b.rect(x, y, w, h, FIELD_BG);
            if edit.widget.focused {
                b.outline(x, y, w, h, 2.0, FG);
            }
        }
    }

    // Measure with the exact font `b.text` is about to draw with —
    // `Quads::text_width` makes the same jar-attached-vs-fallback choice
    // `Quads::text` does. Using `edit.draw_state(None)` (this box's own fixed
    // `MENU_TEXT_ADVANCE`) here was the "cursor gap grows while typing"
    // report: it placed the caret a fixed 6 px per character right of
    // `before`, while the glyphs drawn below (when a real `VanillaFont` is
    // attached) are mostly narrower than that — see `edit_box.rs`'s module
    // docs for the measured cause.
    let font_measure = |s: &str| b.text_width(s, EDIT_TEXT_SCALE);
    let state = edit.draw_state_with(None, Some(&font_measure));
    let colour = edit.text_colour();
    let glyph_h = GLYPH_H as f32 * EDIT_TEXT_SCALE;

    // Selection first, so the glyphs land on top of it. Vanilla inverts the text
    // under the block (`graphics.textHighlight(.., invertHighlightedTextColor)`);
    // a flat fill is the equivalent this pipeline can draw, and it is the same
    // colour the row cursor uses elsewhere.
    if let Some((from, to)) = state.highlight {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        if hi > lo {
            b.rect(lo, state.text_y, hi - lo, glyph_h, ROW_SEL);
        }
    }
    if !state.before.is_empty() {
        b.text(&state.before, state.before_x, state.text_y, EDIT_TEXT_SCALE, colour);
    }
    if !state.after.is_empty() {
        b.text(&state.after, state.after_x, state.text_y, EDIT_TEXT_SCALE, colour);
    }
    if state.show_cursor {
        if state.insert_cursor {
            // `extractInsertCursor`: a 1 px bar, widened to `EDIT_TEXT_SCALE`
            // here for the same reason the height is scaled.
            b.rect(state.cursor_x, state.text_y, EDIT_TEXT_SCALE, glyph_h, colour);
        } else {
            b.rect(
                state.cursor_x,
                state.text_y + glyph_h - EDIT_TEXT_SCALE,
                edit.advance,
                EDIT_TEXT_SCALE,
                colour,
            );
        }
    }
    // The hint (`EditBox.hint`) draws only when the box is empty *and*
    // unfocused (`EditBox.java:438-440`), which is the opposite of a placeholder
    // that vanishes on the first keystroke.
    if let Some(hint) = edit.hint.as_deref() {
        if state.before.is_empty() && state.after.is_empty() && !edit.widget.focused {
            let room = (w - 2.0 * edit_box::BORDER_INSET).max(0.0);
            b.text(
                clip(hint, room, EDIT_TEXT_SCALE),
                state.before_x,
                state.text_y,
                EDIT_TEXT_SCALE,
                colour,
            );
        }
    }
}

/// Longest prefix of `s` that measures at most `max_px` in whatever font `b`
/// draws with. Separate from [`clip`] because that one assumes the fixed
/// advance; measurement and drawing must read the same font (see
/// `docs/vanilla-hud-text.md`).
fn clip_measured<'s>(b: &Quads<'_>, s: &'s str, max_px: f32) -> &'s str {
    let mut fits = 0;
    for (i, ch) in s.char_indices() {
        let end = i + ch.len_utf8();
        if b.text_width(&s[..end], 1.0) > max_px {
            return &s[..fits];
        }
        fits = end;
    }
    s
}

/// A pixel-space quad emitter to NDC for both streams.
///
/// Self-contained for the colour stream (mirrors [`crate::effects`]'s builder)
/// but it *does* borrow two `pub(crate)` HUD types now — `ColourStream` and
/// `push_sprite_quad` — rather than re-deriving their NDC arithmetic. Both are
/// read-only borrows of `hud/item_icon.rs`, which needed no change: the point of
/// not folding this renderer into the HUD's pass stands, while duplicating the
/// vertex maths would be a second definition that could silently drift.
pub(super) struct Quads<'a> {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    sprites: Vec<f32>,
    atlas: Option<&'a GuiAtlas>,
    font: Option<&'a VanillaFont>,
    /// The active clip rect in **logical pixels** as `(x0, y0, x1, y1)`, or
    /// `None` for unclipped. See [`Quads::with_clip`].
    clip: Option<(f32, f32, f32, f32)>,
    /// Scratch buffer for [`Quads::text`]'s clipped path, reused across calls so
    /// a clipped label does not allocate per frame.
    text_scratch: Vec<f32>,
}

impl Quads<'_> {
    pub(super) fn new(w: f32, h: f32) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            sprites: Vec::new(),
            atlas: None,
            font: None,
            clip: None,
            text_scratch: Vec::new(),
        }
    }

    /// Run `body` with everything it emits clipped to `(x, y, w, h)` — this
    /// pipeline's stand-in for vanilla's `enableScissor`/`disableScissor`
    /// (`AbstractSelectionList.java:242-249`, `:212-214`).
    ///
    /// ## Why this is not `set_scissor_rect`
    ///
    /// `set_scissor_rect` appears **nowhere** in this workspace, and adding it
    /// here would mean restructuring the one `"menu-pass"` encoder: it draws the
    /// entire menu in *four* `pass.draw` calls over two vertex streams, so a
    /// GPU scissor would need `MenuGeometry` to record range breaks and the pass
    /// to replay them in order. That is a bigger change than the lists need, and
    /// the ordering between the two streams is already load-bearing (labels are on
    /// the colour stream and must land *on* their button sprite).
    ///
    /// Clipping on the CPU instead costs nothing at draw time and — the deciding
    /// reason — **also clips text**, which a scissor split by stream would too,
    /// but which no cheaper trick does: glyphs bottom out in `ColourStream::rect`
    /// in `hud/item_icon.rs`, one flat quad per horizontal ink run, so they are
    /// not addressable as sprites.
    ///
    /// ## What it clips, and what it cannot
    ///
    /// | primitive | how |
    /// |---|---|
    /// | [`Quads::rect`] (and `outline`, `mosaic`) | rect intersection |
    /// | [`Quads::sprite`] | `dst` **and** `uv` cropped together, both axes |
    /// | [`Quads::text`] | emitted to a scratch buffer, then clipped in NDC |
    ///
    /// The sprite crop is the generalisation of the horizontal-only UV crop the
    /// XP bar already uses (`hud.rs:1302-1312`); doing it on one axis only would
    /// **squash** a favicon instead of cutting it, which is the failure mode worth
    /// naming because it still looks like a picture.
    ///
    /// Nesting replaces rather than intersects, matching vanilla — `enableScissor`
    /// takes absolute bounds and the lists never nest one.
    fn with_clip(&mut self, x: f32, y: f32, w: f32, h: f32, body: impl FnOnce(&mut Self)) {
        let prev = self.clip;
        self.clip = Some((x, y, x + w, y + h));
        body(self);
        self.clip = prev;
    }

    /// Clip a run of already-NDC colour vertices against the active clip and
    /// append what survives — [`Quads::text`]'s path for the vanilla font.
    ///
    /// Reads six vertices at a time (one quad, `FLOATS_PER_VERTEX` each) and takes
    /// their **bounding box**, which is exact for the axis-aligned quads the font
    /// emits. Note NDC y is *inverted* relative to pixels — `1 - 2*py/h` — so the
    /// clip's top edge is the numerically larger y, which is why the two y
    /// comparisons below look backwards and must stay that way.
    fn append_clipped_ndc_quads(&mut self, src: &[f32]) {
        let Some((cx0, cy0, cx1, cy1)) = self.clip else {
            self.verts.extend_from_slice(src);
            return;
        };
        let (w, h) = (self.w, self.h);
        let to_ndc_x = |px: f32| 2.0 * px / w - 1.0;
        let to_ndc_y = |py: f32| 1.0 - 2.0 * py / h;
        // `cy0` is the clip's *top* in pixels, hence its *maximum* in NDC.
        let (kx0, kx1) = (to_ndc_x(cx0), to_ndc_x(cx1));
        let (ky_top, ky_bot) = (to_ndc_y(cy0), to_ndc_y(cy1));

        let stride = FLOATS_PER_VERTEX * 6;
        for quad in src.chunks_exact(stride) {
            let xs = (0..6).map(|i| quad[i * FLOATS_PER_VERTEX]);
            let ys = (0..6).map(|i| quad[i * FLOATS_PER_VERTEX + 1]);
            let (mut x0, mut x1) = (f32::INFINITY, f32::NEG_INFINITY);
            for x in xs {
                x0 = x0.min(x);
                x1 = x1.max(x);
            }
            let (mut y_bot, mut y_top) = (f32::INFINITY, f32::NEG_INFINITY);
            for y in ys {
                y_bot = y_bot.min(y);
                y_top = y_top.max(y);
            }
            let nx0 = x0.max(kx0);
            let nx1 = x1.min(kx1);
            let ntop = y_top.min(ky_top);
            let nbot = y_bot.max(ky_bot);
            if nx1 <= nx0 || ntop <= nbot {
                continue;
            }
            let c = [quad[2], quad[3], quad[4], quad[5]];
            for (vx, vy) in [
                (nx0, ntop),
                (nx1, ntop),
                (nx1, nbot),
                (nx0, ntop),
                (nx1, nbot),
                (nx0, nbot),
            ] {
                self.verts
                    .extend_from_slice(&[vx, vy, c[0], c[1], c[2], c[3]]);
            }
        }
    }

    /// Intersect a pixel rect with the active clip, or `None` if nothing is left.
    fn clipped_rect(&self, x: f32, y: f32, w: f32, h: f32) -> Option<(f32, f32, f32, f32)> {
        let Some((cx0, cy0, cx1, cy1)) = self.clip else {
            return Some((x, y, w, h));
        };
        let x0 = x.max(cx0);
        let y0 = y.max(cy0);
        let x1 = (x + w).min(cx1);
        let y1 = (y + h).min(cy1);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
    }

    /// Whether the bound atlas can draw `id` at all. Distinct from "did the
    /// draw emit anything", which is what makes the jar-less fallback a choice
    /// rather than a silent nothing.
    fn has_sprite(&self, id: &str) -> bool {
        self.atlas.is_some_and(|a| a.contains(id))
    }

    /// Emit a GUI sprite scaled into `(x, y, w, h)`, honouring its `.mcmeta`
    /// scaling — nine-slice borders included, read from the pack by
    /// [`GuiAtlas::geometry`]. A no-op with no atlas or an unknown id.
    fn sprite(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        let quads: Vec<GuiSpriteQuad> = match self.atlas {
            Some(a) => a.geometry(id, x, y, w, h),
            None => return,
        };
        for q in quads {
            // Each nine-slice piece is cropped independently, against its own
            // `dst` — the borders and the centre sample different UV spans, so a
            // single crop applied to the whole sprite would smear one across
            // another.
            if let Some(q) = self.crop_sprite_quad(q) {
                push_sprite_quad(&mut self.sprites, self.w, self.h, q, c);
            }
        }
    }

    /// Crop one sprite quad's destination **and** its UVs to the active clip, so
    /// the texture is cut rather than squashed. See [`Quads::with_clip`].
    fn crop_sprite_quad(&self, q: GuiSpriteQuad) -> Option<GuiSpriteQuad> {
        let [dx, dy, dw, dh] = q.dst;
        if dw <= 0.0 || dh <= 0.0 {
            return None;
        }
        let (nx, ny, nw, nh) = self.clipped_rect(dx, dy, dw, dh)?;
        if self.clip.is_none() {
            return Some(q);
        }
        // The fraction of the original destination each edge moved by, applied to
        // the UV span so the visible texels stay put.
        let [u0, v0] = q.uv_min;
        let [u1, v1] = q.uv_max;
        let (su, sv) = (u1 - u0, v1 - v0);
        let fx0 = (nx - dx) / dw;
        let fx1 = (nx + nw - dx) / dw;
        let fy0 = (ny - dy) / dh;
        let fy1 = (ny + nh - dy) / dh;
        Some(GuiSpriteQuad {
            dst: [nx, ny, nw, nh],
            uv_min: [u0 + su * fx0, v0 + sv * fy0],
            uv_max: [u0 + su * fx1, v0 + sv * fy1],
        })
    }

    /// Width of `s` in the font this builder will actually *draw* with — the
    /// proportional vanilla one when attached, the fixed 5×7 advance otherwise.
    pub(super) fn text_width(&self, s: &str, scale: f32) -> f32 {
        match self.font {
            Some(f) => f.width(s, scale),
            None => text_px(s, scale),
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // Clipped in *pixel* space, before the NDC conversion, so the intersection
        // is stated in the same units the caller reasoned about.
        let Some((x, y, w, h)) = self.clipped_rect(x, y, w, h) else {
            return;
        };
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let (x0, y0) = to_ndc(x, y);
        let (x1, y1) = to_ndc(x + w, y + h);
        let mut v = |vx: f32, vy: f32| {
            self.verts
                .extend_from_slice(&[vx, vy, c[0], c[1], c[2], c[3]]);
        };
        v(x0, y0);
        v(x1, y0);
        v(x1, y1);
        v(x0, y0);
        v(x1, y1);
        v(x0, y1);
    }

    /// Four thin rects forming a border just inside `(x, y, w, h)`.
    fn outline(&mut self, x: f32, y: f32, w: f32, h: f32, t: f32, c: [f32; 4]) {
        self.rect(x, y, w, t, c);
        self.rect(x, y + h - t, w, t, c);
        self.rect(x, y, t, h, c);
        self.rect(x + w - t, y, t, h, c);
    }

    /// One string at `(x, y)` (top-left of the first glyph).
    ///
    /// With a [`VanillaFont`] attached this is vanilla text — real glyphs, real
    /// advances, and the 1 px 25 %-brightness drop shadow — through the exact
    /// same code path the HUD uses. Without one it is the fixed-advance 5×7
    /// debug bitmap, unshadowed, as before. Measurement goes through
    /// [`Quads::text_width`], which picks whichever of the two this will draw
    /// with, so a centred label can never be laid out against the other font.
    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        if let Some(f) = self.font {
            let (w, h) = (self.w, self.h);
            if self.clip.is_none() {
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
                return;
            }
            // Clipped: draw into scratch, then cut the emitted quads in NDC.
            //
            // `VanillaFont::draw` takes a concrete `ColourStream` (it is a struct,
            // not a trait — `hud/item_icon.rs:666`), so there is no seam to inject
            // a clipping stream through, and `hud/` is not this change's to edit.
            // Post-processing works because every glyph reaches the stream as an
            // **axis-aligned** quad — one per horizontal ink run per texel row
            // (`hud/vanilla_font.rs:512-519`) — so a bounding box over its six
            // vertices reconstructs the rect losslessly.
            let mut scratch = core::mem::take(&mut self.text_scratch);
            scratch.clear();
            f.draw(
                &mut ColourStream {
                    verts: &mut scratch,
                    w,
                    h,
                },
                s,
                x,
                y,
                scale,
                c,
            );
            self.append_clipped_ndc_quads(&scratch);
            self.text_scratch = scratch;
            return;
        }
        let mut cursor = x;
        for ch in s.chars() {
            if ch != ' ' {
                for (ry, row) in glyph_rows(ch).iter().enumerate() {
                    for rx in 0..GLYPH_W {
                        if (row >> (GLYPH_W - 1 - rx)) & 1 == 1 {
                            self.rect(
                                cursor + rx as f32 * scale,
                                y + ry as f32 * scale,
                                scale,
                                scale,
                                c,
                            );
                        }
                    }
                }
            }
            cursor += advance(scale);
        }
    }

    /// One list of styled spans at `(x, y)` — [`Quads::text`]'s coloured twin.
    ///
    /// `base` is the colour for a span the server left uncoloured, so a MOTD with
    /// no colour of its own renders exactly as it did before this existed. The
    /// clipping story is identical to [`Quads::text`]'s and for the same reason:
    /// every glyph reaches the stream as an axis-aligned quad, so the emitted
    /// vertices can be cut in NDC afterwards.
    fn text_spans(
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
            if self.clip.is_none() {
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
            let mut scratch = core::mem::take(&mut self.text_scratch);
            scratch.clear();
            f.draw_spans(
                &mut ColourStream {
                    verts: &mut scratch,
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
            self.append_clipped_ndc_quads(&scratch);
            self.text_scratch = scratch;
            return;
        }
        // Jar-less debug font: colour per span, fixed advance, as `text` does.
        let mut cursor = x;
        for span in spans {
            let rgb = span
                .style
                .color
                .map_or(base, crate::hud::vanilla_font::text_color_rgb);
            let c = [rgb[0], rgb[1], rgb[2], alpha];
            self.text(&span.text, cursor, y, scale, c);
            cursor += text_px(&span.text, scale);
        }
    }

    /// A favicon mosaic as a `side`×`side` px square of coloured cells.
    fn mosaic(&mut self, m: &FaviconMosaic, x: f32, y: f32, side: f32) {
        if m.size == 0 {
            return;
        }
        let cell = side / m.size as f32;
        for (i, c) in m.cells.iter().enumerate() {
            if c[3] <= 0.0 {
                continue;
            }
            let cx = (i % m.size) as f32;
            let cy = (i / m.size) as f32;
            self.rect(x + cx * cell, y + cy * cell, cell, cell, *c);
        }
    }
}

