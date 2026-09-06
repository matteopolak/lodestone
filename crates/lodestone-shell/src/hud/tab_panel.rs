//! Layout model for the Tab player-list overlay.
//!
//! The panel is kept separate from the HUD's draw builder so the geometry used
//! by rendering and pixel gates remains one small, reusable calculation.

use super::TAB_LINE_H;

/// Maximum rows allowed in one column before the list grows another column.
pub(super) const TAB_MAX_ROWS_PER_COL: usize = 20;

/// Horizontal gap between two columns — the literal `5` in
/// `xo = xxo + col * slotWidth + col * 5`.
pub(super) const TAB_COL_GAP: f32 = 5.0;

/// The 9 px a row reserves for its 8×8 player face, plus the 1 px vanilla leaves
/// between the face and the name (`xo += 9` after the face blit).
pub(super) const TAB_HEAD_W: f32 = 9.0;

/// The per-row slack in vanilla's slot-width estimate — the literal `13` in
/// `cols * ((showHead ? 9 : 0) + maxNameWidth + widthForScore + 13)`. It is what
/// leaves room for the 10 px ping icon plus a pixel either side.
pub(super) const TAB_ROW_SLACK: f32 = 13.0;

/// The margin vanilla keeps clear either side — the `screenWidth - 50` cap on
/// both the slot-width estimate and the header/footer wrap width.
pub(super) const TAB_SCREEN_INSET: f32 = 50.0;

/// The overlay's top edge — `yyo = 10`.
pub(super) const TAB_TOP: f32 = 10.0;

/// The ping icon's drawn size and its offset from the slot's right edge —
/// `blitSprite(sprite, xo + slotWidth - 11, yo, 10, 8)`.
pub(super) const TAB_PING_W: f32 = 10.0;
/// See [`TAB_PING_W`].
pub(super) const TAB_PING_H: f32 = 8.0;
/// See [`TAB_PING_W`].
pub(super) const TAB_PING_INSET: f32 = 11.0;

/// The plate behind the header, the rows and the footer — vanilla's
/// `Integer.MIN_VALUE`, i.e. `0x80000000`: black at alpha `128`.
pub(super) const TAB_PLATE: [f32; 4] = [0.0, 0.0, 0.0, 0x80 as f32 / 255.0];

/// The per-row fill — `options.getBackgroundColor(553648127)`, i.e. `0x20FFFFFF`:
/// **white** at alpha `32`, not another black wash. Getting this wrong is what
/// makes the rows read as one flat block instead of a striped list.
pub(super) const TAB_ROW_FILL: [f32; 4] = [1.0, 1.0, 1.0, 0x20 as f32 / 255.0];

/// A row's ink. Opaque white, or `0x90FFFFFF` for a spectator
/// (`-1862270977`).
pub(super) const TAB_INK: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// See [`TAB_INK`].
pub(super) const TAB_INK_SPECTATOR: [f32; 4] = [1.0, 1.0, 1.0, 0x90 as f32 / 255.0];

/// The Tab player-list overlay's geometry, following the server-list screen's
/// column growth, slot sizing, and centered plate rules.
///
/// **Exists so the draw and its gate share one expression rather than two that
/// agree today.** A pixel gate that recomputed `y` from its own copy of this
/// arithmetic would keep passing after the panel moved — a control whose premise
/// is false in the safe-looking direction. `build_inner` constructs one of these
/// and draws from it; a gate constructs one from the same inputs and measures
/// against it.
///
/// Every division below is vanilla's **integer** division, floored here for that
/// reason: `slot_w` in particular is `min(...) / cols`, and letting it stay
/// fractional would put column 1 half a pixel off vanilla at most widths.
/// `pub` rather than `pub(crate)` so an **integration** gate can derive the
/// overlay's rect from this constructor instead of restating it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabPanel {
    /// Number of columns the rows are split into.
    pub cols: usize,
    /// Rows **per column** — vanilla's own row count, which is also the stride the
    /// `col = i / rows` / `row = i % rows` pair indexes with.
    pub rows: usize,
    /// One column's width.
    pub slot_w: f32,
    /// Left edge of column 0 — vanilla's own column-origin x-coordinate.
    pub x: f32,
    /// Top of the **rows** block, after any header — vanilla's own row-origin
    /// y-coordinate at the point the row loop starts.
    pub rows_top: f32,
    /// Top of the header block, or `rows_top` when there is no header.
    pub header_top: f32,
    /// Top of the footer block. Only meaningful when there is a footer.
    pub footer_top: f32,
    /// The widest thing on screen — vanilla's own max-line-width value, which is the row
    /// block's own width *widened* by any header or footer line that overflows
    /// it. Every plate spans this, centred on the screen.
    pub max_line_width: f32,
    /// The screen (logical canvas) width the layout was built for.
    pub screen_w: f32,
}

impl TabPanel {
    /// Lay the overlay out for a logical canvas and a content census.
    ///
    /// `max_name_width` and `widest_banner` must be measured with the **same**
    /// font and scale the draw uses; they are the only inputs vanilla takes from
    /// its font, and passing a differently-measured pair is how a layout and its
    /// draw silently disagree.
    pub fn new(
        screen_w: f32,
        slots: usize,
        show_head: bool,
        max_name_width: f32,
        header_len: usize,
        widest_banner: f32,
    ) -> Self {
        // `for (cols = 1; rows > 20; rows = (slots + cols - 1) / cols) { cols++; }`
        // Read the loop in its own order: increment the column count before
        // recomputing the row count.
        let mut cols = 1usize;
        let mut rows = slots;
        while rows > TAB_MAX_ROWS_PER_COL {
            cols += 1;
            rows = slots.div_ceil(cols);
        }
        let head_w = if show_head { TAB_HEAD_W } else { 0.0 };
        // The score column is absent because the shell has no display objective.
        let estimate = cols as f32 * (head_w + max_name_width + TAB_ROW_SLACK);
        let slot_w = (estimate.min(screen_w - TAB_SCREEN_INSET) / cols as f32).floor();
        let block_w = slot_w * cols as f32 + (cols as f32 - 1.0) * TAB_COL_GAP;
        let x = (screen_w * 0.5).floor() - (block_w * 0.5).floor();
        let max_line_width = block_w.max(widest_banner);
        let header_top = TAB_TOP;
        let rows_top = if header_len > 0 {
            TAB_TOP + header_len as f32 * TAB_LINE_H + 1.0
        } else {
            TAB_TOP
        };
        let footer_top = rows_top + rows as f32 * TAB_LINE_H + 1.0;
        Self {
            cols,
            rows,
            slot_w,
            x,
            rows_top,
            header_top,
            footer_top,
            max_line_width,
            screen_w,
        }
    }

    /// Left edge of a plate — half the screen width, minus half the max line
    /// width, minus one.
    pub fn plate_x(&self) -> f32 {
        (self.screen_w * 0.5).floor() - (self.max_line_width * 0.5).floor() - 1.0
    }

    /// A plate's width, including the one-pixel edges on both sides.
    pub fn plate_w(&self) -> f32 {
        (self.screen_w * 0.5).floor() + (self.max_line_width * 0.5).floor() + 1.0
            - self.plate_x()
    }

    /// Top-left of row `i`'s slot, in column-major order.
    pub fn slot_origin(&self, i: usize) -> [f32; 2] {
        let col = i / self.rows.max(1);
        let row = i % self.rows.max(1);
        [
            self.x + col as f32 * (self.slot_w + TAB_COL_GAP),
            self.rows_top + row as f32 * TAB_LINE_H,
        ]
    }

    /// Baseline of header line `i`.
    pub fn header_y(&self, i: usize) -> f32 {
        self.header_top + i as f32 * TAB_LINE_H
    }

    /// Baseline of footer line `i`.
    pub fn footer_y(&self, i: usize) -> f32 {
        self.footer_top + i as f32 * TAB_LINE_H
    }

    /// x for a line of width `text_w` centred on the screen.
    pub fn centred_x(&self, text_w: f32) -> f32 {
        (self.screen_w * 0.5).floor() - (text_w * 0.5).floor()
    }
}
