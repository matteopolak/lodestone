//! The hover tooltip for a container slot — vanilla's `ItemStack.getTooltipLines`
//! plus `Screen.renderTooltip`.
//!
//! ## What it is
//!
//! One function, [`emit_tooltip`], called at the very end of
//! [`ContainerGeometry::build_inner`](super::ContainerGeometry): given the frame's
//! cursor it resolves the hovered slot, gathers the lines vanilla would show, and
//! appends a background and the text to the colour stream.
//!
//! ## How it works
//!
//! It emits into the **tail** of the colour stream, after the carried stack, and
//! `ContainerRenderer` draws that range last of all — after the slot icons, after
//! the background sprites, after the carried stratum's own model and sprite
//! passes. So the tooltip is on top of everything by submission order, which is
//! the only z this GUI path has. `build_inner`'s own comment used to end "and
//! below the tooltip (which this client does not draw yet)"; this is that.
//!
//! ## How to change it
//!
//! Adding a line means adding to [`tooltip_lines`]. Two gotchas:
//!
//! * **The background is the pre-1.20.2 gradient form, not 26.2's sprites.**
//!   `TooltipRenderUtil.extractTooltipBackground` blits `tooltip/background` and
//!   `tooltip/frame`, which are nine-slice sprites. Reaching them from here would
//!   need a nine-slicing atlas above the carried stratum, and the only stream
//!   above it is the untextured colour one — so this draws the fills-and-gradients
//!   background vanilla used for a decade and which the sprites reproduce. If a
//!   sprite pass ever lands above the carried stratum, this is the code to
//!   replace, and [`TOOLTIP_BG`]'s doc has the sprite ids.
//! * **No lore lines.** `minecraft:lore` is not decoded anywhere in this tree
//!   (there is no `LORE_COMPONENT`, and `ComponentValue` has no list-of-`Text`
//!   variant), so there is nothing to read. Checked, not assumed. Enchantment
//!   lines are absent for a different reason: `ItemEnchantment::id` is a
//!   *session-scoped numeric registry id* with no name table on the client, so a
//!   line for it would read "Enchantment #12" — a fabrication, which this module
//!   declines the same way `menu::options` declines a row it cannot honour.
//!
//! ## Dependencies
//!
//! [`lodestone_game::item::styled_hover_name`] for the title line, and
//! [`super::layout::hit_test_with_scale`] for the hovered slot — the *same*
//! function the click path uses, so a tooltip can never appear for a slot a click
//! would resolve elsewhere.

use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;

use super::builder::Builder;
use super::layout::{MenuHit, hit_test_with_scale};

/// `TooltipRenderUtil.MOUSE_OFFSET` (`TooltipRenderUtil.java:12`) — the tooltip's
/// text origin sits `(+12, -12)` from the cursor
/// (`DefaultTooltipPositioner.java:14`).
const MOUSE_OFFSET: f32 = 12.0;
/// `TooltipRenderUtil.PADDING` (`:13`), the same `3` on all four sides.
const PADDING: f32 = 3.0;
/// The line pitch between tooltip lines — vanilla's `10` in
/// `renderTooltipInternal`'s `if (i == 0) tooltipHeight += 2` / `+= 10` walk.
const LINE_PITCH: f32 = 10.0;
/// Extra gap after the **first** line only, so the name is separated from the
/// body — vanilla's `if (lines.size() > 1) tooltipHeight += 2`.
const TITLE_GAP: f32 = 2.0;
/// One glyph line's height, for the box's own height arithmetic.
const LINE_H: f32 = 9.0;

/// Vanilla's tooltip fill, `0xF0100010` in ARGB — a near-black, near-opaque
/// purple.
///
/// This and the two border colours are the pre-sprite constants from
/// `GuiGraphics.renderTooltipInternal`; 26.2 draws `tooltip/background` and
/// `tooltip/frame` instead (`TooltipRenderUtil.java:9-10`), which are nine-slice
/// PNGs whose art reproduces exactly this look. See the module doc for why the
/// sprites are out of reach from this stream.
const TOOLTIP_BG: [f32; 4] = [16.0 / 255.0, 0.0, 16.0 / 255.0, 240.0 / 255.0];
/// The border gradient's top colour, `0x505000FF`.
const BORDER_TOP: [f32; 4] = [80.0 / 255.0, 0.0, 1.0, 80.0 / 255.0];
/// The border gradient's bottom colour, `0x5028007F`.
const BORDER_BOTTOM: [f32; 4] = [80.0 / 255.0, 0.0, 127.0 / 255.0, 40.0 / 255.0];
/// The title line's colour. Vanilla tints by rarity; this build carries no rarity
/// data (see [`lodestone_game::item::styled_hover_name`]'s own note), and common
/// — i.e. white — is right for the overwhelming majority of items.
const NAME_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// `ChatFormatting.DARK_GRAY`, `0x555555` — what the advanced lines use
/// (`ItemStack.java:924, 927`).
const DARK_GRAY: [f32; 4] = [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0];

/// One tooltip line: its text and its colour.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TooltipLine {
    /// The text, already resolved to words.
    pub text: String,
    /// The colour to draw it in.
    pub colour: [f32; 4],
}

/// The lines vanilla would show for `stack` — `ItemStack.getTooltipLines`, in its
/// order, restricted to what this build actually has data for.
///
/// The first line is always the hover name. `advanced` adds
/// `addDetailsToTooltip`'s `isAdvanced()` block (`ItemStack.java:919-929`), in
/// vanilla's order: durability first (**only when damaged**), then the item id,
/// then the component count.
///
/// The strings are English literals rather than language-table lookups, which is
/// the same documented gap `geometry.rs`'s own label code carries: no language
/// table reaches this module. `item.durability` is `"Durability: %s / %s"` and
/// `item.components` is `"%s Component(s)"` in `en_us.json`.
pub(super) fn tooltip_lines(stack: &ItemStack, advanced: bool) -> Vec<TooltipLine> {
    let mut lines = vec![TooltipLine {
        // `&|_| None`: no language table here, so this resolves through
        // `base_display_name`'s humanised fallback — "Diamond Sword" for
        // `minecraft:diamond_sword`. Right for every vanilla item whose id is its
        // name in snake_case, which is nearly all of them.
        text: lodestone_game::item::styled_hover_name(stack, &|_| None),
        colour: NAME_COLOUR,
    }];
    if !advanced {
        return lines;
    }
    // `isDamaged()` is `damage > 0`, and the line needs `getMaxDamage()` too —
    // read through the component rather than a helper because `ItemStack` exposes
    // `is_damageable`/`is_damaged` as bools and no accessor for the value.
    if let Some(damage) = stack.damage().filter(|d| *d > 0)
        && let Some(max) = stack
            .components()
            .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
    {
        let max = i32::try_from(max).unwrap_or(i32::MAX);
        lines.push(TooltipLine {
            text: format!("Durability: {} / {}", max - damage, max),
            colour: DARK_GRAY,
        });
    }
    lines.push(TooltipLine {
        text: stack.item().to_string(),
        colour: DARK_GRAY,
    });
    let components = stack.components().len();
    if components > 0 {
        lines.push(TooltipLine {
            text: format!("{components} Component(s)"),
            colour: DARK_GRAY,
        });
    }
    lines
}

/// Append the hovered slot's tooltip to `b`'s colour stream, if there is one.
///
/// A no-op — and therefore free for every existing caller — unless *all* of:
/// the frame carried a cursor, the cursor is over a slot, that slot holds a
/// stack, nothing is on the cursor, and a font is attached.
///
/// # The five preconditions, and why each is one
///
/// * **cursor** — `ContainerFrame::cursor` is `None` unless a caller opted in via
///   `with_cursor`, exactly as the carried-stack draw requires.
/// * **over a slot** — through [`hit_test_with_scale`], the same call the click
///   path makes. The alternative (re-deriving the slot rects here) is how a
///   tooltip comes to describe a different slot than a click would act on.
/// * **slot holds a stack** — vanilla's
///   `if (hoveredSlot != null && hoveredSlot.hasItem())`
///   (`AbstractContainerScreen.java:202`).
/// * **nothing carried** — the same line's `&& carried.isEmpty()`. This is also
///   what makes the submission-order layering sound: the tooltip is emitted after
///   the carried stack, so if both could show at once the tooltip would cover it.
/// * **font** — with no `VanillaFont` there is nothing to measure the box against,
///   and a box sized off the 5×7 debug font would be visibly wrong. A jar-less run
///   draws no tooltip rather than a mis-sized one.
pub(super) fn emit_tooltip(
    b: &mut Builder<'_>,
    menu: &Menu,
    cursor: Option<[f32; 2]>,
    advanced: bool,
    gui_scale: u32,
    width: u32,
    height: u32,
    canvas: (f32, f32),
) {
    let Some([cx, cy]) = cursor else { return };
    if menu.carried().is_some() {
        return;
    }
    let Some(font) = b.font else { return };
    let MenuHit::Slot(index) = hit_test_with_scale(menu, gui_scale, width, height, cx, cy) else {
        return;
    };
    let Some(stack) = menu.slot_item(index) else {
        return;
    };
    let lines = tooltip_lines(stack, advanced);
    if lines.is_empty() {
        return;
    }

    // `cursor` is physical viewport space (`hit_test`'s), this builder is the
    // logical canvas — the same division the carried-stack draw performs, and for
    // the same reason.
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let (cx, cy) = (cx / scale, cy / scale);

    let text_w = lines
        .iter()
        .map(|l| font.width(&l.text, 1.0))
        .fold(0.0f32, f32::max);
    // The height walk: one `LINE_H` per line at `LINE_PITCH`, plus vanilla's extra
    // 2 px under the title when there is a body.
    let text_h = if lines.len() > 1 {
        LINE_H + TITLE_GAP + (lines.len() - 1) as f32 * LINE_PITCH
    } else {
        LINE_H
    };

    // `DefaultTooltipPositioner.positionTooltip` (`:13-27`), verbatim: `(+12,
    // -12)` from the cursor, then flip left of the cursor if it would overflow the
    // right edge (floored at 4, never off the left), then lift it so the *padded*
    // height fits.
    let (sw, sh) = canvas;
    let mut tx = cx + MOUSE_OFFSET;
    let mut ty = cy - MOUSE_OFFSET;
    if tx + text_w > sw {
        tx = (tx - 24.0 - text_w).max(4.0);
    }
    let padded_h = text_h + PADDING;
    if ty + padded_h > sh {
        ty = sh - padded_h;
    }

    // The background: vanilla's five fills for the body (which together are one
    // rect grown 1 px on each axis into the border ring) and four for the border.
    // Written as the grown body plus a 1 px gradient ring rather than nine
    // separate `fillGradient` calls — the same pixels, and the arithmetic is
    // legible instead of nine near-identical lines.
    let (bx, by) = (tx - PADDING, ty - PADDING);
    let (bw, bh) = (text_w + PADDING * 2.0, text_h + PADDING * 2.0);
    b.rect_px(bx - 1.0, by, bw + 2.0, bh, TOOLTIP_BG);
    b.rect_px(bx, by - 1.0, bw, bh + 2.0, TOOLTIP_BG);
    // The border ring, gradient down the two vertical edges and flat on the two
    // horizontal ones — `BORDER_TOP` at the top, `BORDER_BOTTOM` at the bottom,
    // which is what makes the frame read as lit from above.
    b.gradient_rect_px(bx, by + 1.0, 1.0, bh - 2.0, BORDER_TOP, BORDER_BOTTOM);
    b.gradient_rect_px(
        bx + bw - 1.0,
        by + 1.0,
        1.0,
        bh - 2.0,
        BORDER_TOP,
        BORDER_BOTTOM,
    );
    b.rect_px(bx, by, bw, 1.0, BORDER_TOP);
    b.rect_px(bx, by + bh - 1.0, bw, 1.0, BORDER_BOTTOM);

    let mut y = ty;
    for (i, line) in lines.iter().enumerate() {
        b.shadowed_label(&line.text, tx, y, 1.0, line.colour);
        y += LINE_PITCH;
        if i == 0 {
            y += TITLE_GAP;
        }
    }
}
