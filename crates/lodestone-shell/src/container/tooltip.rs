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
//! * **No `minecraft:lore` lines.** That component is not decoded anywhere in
//!   this tree (there is no `LORE_COMPONENT`, and `ComponentValue` has no
//!   list-of-`Text` variant), so there is nothing to read. Checked, not assumed.
//!   Enchantment lines are absent for a different reason: `ItemEnchantment::id`
//!   is a *session-scoped numeric registry id* with no name table on the client,
//!   so a line for it would read "Enchantment that fix" — a fabrication, which
//!   this module declines the same way `menu::options` declines a row it cannot
//!   honour. **Potion effect lines are a different source and are implemented**
//!   ([`potion_lore_lines`]): `PotionContents.addPotionTooltip` composes them
//!   from the stack's own `minecraft:potion_contents`, not from `minecraft:lore`,
//!   and the potion registry (unlike `minecraft:enchantment`) is fixed and
//!   built-in, so it carries no session-scoped id problem.
//!
//! ## Dependencies
//!
//! [`lodestone_game::item::styled_hover_name`] for the title line, except a
//! potion-family stack's, which [`hover_name`] resolves through
//! `lodestone_data::potion` instead — see that function's own doc for why. The
//! hovered slot is **not** resolved here — `build_inner` resolves it once
//! (through `super::layout::hit_test_with_book`, the same function the click path
//! uses) and passes it in, so a tooltip can never appear for a slot the
//! highlight, or a click, would resolve elsewhere.

use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;

use super::builder::Builder;

/// `TooltipRenderUtil.MOUSE_OFFSET` (`TooltipRenderUtil.java`) — the tooltip's
/// text origin sits `(+12, -12)` from the cursor
/// (`DefaultTooltipPositioner.java`).
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
/// `tooltip/frame` instead (`TooltipRenderUtil.java`), which are nine-slice
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
/// (`ItemStack.java, 927`).
const DARK_GRAY: [f32; 4] = [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0];
/// `ChatFormatting.GRAY`, `0xAAAAAA` — `PotionContents.NO_EFFECT`'s colour
/// (`Component.translatable("effect.none").withStyle(ChatFormatting.GRAY)`).
const GRAY: [f32; 4] = [170.0 / 255.0, 170.0 / 255.0, 170.0 / 255.0, 1.0];
/// `ChatFormatting.BLUE`, `0x5555FF` — `MobEffectCategory::BENEFICIAL`/`NEUTRAL`'s
/// tooltip colour, and a positive attribute-modifier line's colour
/// (`PotionContents.addPotionTooltip`'s `ChatFormatting.BLUE` branch).
const BLUE: [f32; 4] = [85.0 / 255.0, 85.0 / 255.0, 1.0, 1.0];
/// `ChatFormatting.RED`, `0xFF5555` — `MobEffectCategory::HARMFUL`'s tooltip
/// colour, and a negative attribute-modifier line's colour.
const RED: [f32; 4] = [1.0, 85.0 / 255.0, 85.0 / 255.0, 1.0];
/// `ChatFormatting.DARK_PURPLE`, `0xAA00AA` — `potion.whenDrank`'s
/// (`"When Applied:"`) colour.
const DARK_PURPLE: [f32; 4] = [170.0 / 255.0, 0.0, 170.0 / 255.0, 1.0];

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
/// `addDetailsToTooltip`'s `isAdvanced()` block (`ItemStack.java`), in
/// vanilla's order: durability first (**only when damaged**), then the item id,
/// then the component count.
///
/// The strings are English literals rather than language-table lookups, which is
/// the same documented gap `geometry.rs`'s own label code carries: no language
/// table reaches this module. `item.durability` is `"Durability: %s / %s"` and
/// `item.components` is `"%s Component(s)"` in `en_us.json`.
pub(super) fn tooltip_lines(stack: &ItemStack, advanced: bool) -> Vec<TooltipLine> {
    let mut lines = vec![TooltipLine {
        text: hover_name(stack),
        colour: NAME_COLOUR,
    }];
    // `PotionContents.addToTooltip`/`ItemEnchantments.addToTooltip` both run
    // unconditionally — neither is gated behind `isAdvanced()` the way
    // durability/id/component-count are, so these lines belong before the
    // `advanced` early-return, not after it. A stack is never both, so at most
    // one of the two `extend`s below is ever non-empty.
    lines.extend(potion_lore_lines(stack));
    lines.extend(enchantment_lore_lines(stack));
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

/// The tooltip title: [`lodestone_game::item::styled_hover_name`], except for a
/// potion-family stack carrying a `minecraft:potion_contents` potion id, whose real
/// title is `PotionContents.getName(prefix)`
/// (`.cache/mc/26.2/src/net/minecraft/world/item/alchemy/PotionContents.java`,
/// read through `PotionItem.getName`/`TippedArrowItem.getName`) — composed from the
/// potion's own registry id, not the bare item id.
///
/// Resolving that needs `lodestone_data::potion`, which
/// `lodestone_game::item::styled_hover_name` cannot depend on — this build's
/// version-free canonical crate must not know about protocol-776-specific data,
/// the same reason [`lodestone_model::ItemComponents::potion_color`] is stored
/// pre-mixed rather than resolved on read. So the special case lives here, in the
/// one crate `creative.rs`'s own potion-colour resolution already establishes is
/// allowed to import [`lodestone_data`].
///
/// A custom name still wins outright: `ItemStack.getHoverName()` checks
/// `DataComponents.CUSTOM_NAME` before ever calling `Item.getName`, and
/// `PotionItem`/`TippedArrowItem` only override the latter.
fn hover_name(stack: &ItemStack) -> String {
    if stack.custom_name().is_none()
        && let Some(potion_id) = stack.potion_effect_id()
        && let Some(name) = lodestone_data::potion::potion_item_display_name(stack.item().path(), potion_id)
    {
        return name.to_string();
    }
    // `&|_| None`: no language table here, so a non-potion item resolves through
    // `base_display_name`'s humanised fallback — "Diamond Sword" for
    // `minecraft:diamond_sword`. Right for every vanilla item whose id is its
    // name in snake_case, which is nearly all of them.
    lodestone_game::item::styled_hover_name(stack, &|_| None)
}

/// `PotionContents.addPotionTooltip` (`PotionContents.java`) for a potion-family
/// stack: empty for every other item (`stack.potion_effect_id()` is `None` unless
/// [`super::creative::potion_color_for`] — or, on a real join, a decode this build
/// does not yet perform — attached one; see this module's own doc for that gap).
///
/// Two clauses, in vanilla's order:
///
/// 1. One line per effect: [`lodestone_data::potion::potion_effect_entries`]'s
///    `effect_name`, a Roman-numeral amplifier suffix when `amplifier > 0`
///    (`getPotionDescription`'s `amplifier > 0` gate — `0` renders no numeral at
///    all, not `"I"`), and a `(m:ss)` duration suffix when `duration_ticks > 20`
///    (`!effect.endsWithin(20)`) — an instant effect (`healing`/`harming`) prints
///    no duration. Coloured by [`lodestone_data::potion::PotionEffectEntry::harmful`]
///    (`MobEffectCategory::getTooltipFormatting`). A potion with **no** effects
///    (a water bottle) prints vanilla's own `"No Effects"` line instead of an
///    empty list — the `noEffects` branch, not a fallback this build invented.
/// 2. When at least one effect carries an attribute modifier
///    ([`lodestone_data::potion::potion_attribute_modifiers`]), a blank line, a
///    `"When Applied:"` header, then one signed magnitude line per modifier
///    (`attribute.modifier.plus.*`/`take.*`). Most potions in this build's
///    46-entry registry carry none, in which case this whole clause is skipped —
///    `swiftness`/`slowness`/`strength`/`weakness`/`luck`/`leaping`/`invisibility`
///    are the ones that do.
fn potion_lore_lines(stack: &ItemStack) -> Vec<TooltipLine> {
    let Some(potion_id) = stack.potion_effect_id() else {
        return Vec::new();
    };
    let entries = lodestone_data::potion::potion_effect_entries(potion_id);
    let mut lines = Vec::new();
    if entries.is_empty() {
        lines.push(TooltipLine {
            text: "No Effects".to_string(),
            colour: GRAY,
        });
        return lines;
    }
    for entry in &entries {
        let mut text = entry.effect_name.to_string();
        let numeral = potency_numeral(entry.amplifier);
        if !numeral.is_empty() {
            text = format!("{text} {numeral}");
        }
        if entry.duration_ticks > 20 {
            text = format!("{text} ({})", format_duration_mmss(entry.duration_ticks));
        }
        lines.push(TooltipLine {
            text,
            colour: if entry.harmful { RED } else { BLUE },
        });
    }
    let modifiers = lodestone_data::potion::potion_attribute_modifiers(potion_id);
    if !modifiers.is_empty() {
        lines.push(TooltipLine {
            text: String::new(),
            colour: NAME_COLOUR,
        });
        lines.push(TooltipLine {
            text: "When Applied:".to_string(),
            colour: DARK_PURPLE,
        });
        for modifier in &modifiers {
            let magnitude = format_attribute_amount(modifier.amount.abs(), modifier.percent);
            let sign = if modifier.amount < 0.0 { "-" } else { "+" };
            let suffix = if modifier.percent { "%" } else { "" };
            lines.push(TooltipLine {
                text: format!("{sign}{magnitude}{suffix} {}", modifier.attribute_name),
                colour: if modifier.amount < 0.0 { RED } else { BLUE },
            });
        }
    }
    lines
}

/// `en_us.json`'s `potion.potency.<n>` table, `0..=5` — the only amplifiers a
/// potion in this build's 46-entry registry can carry
/// (`strong_turtle_master`'s `slowness` component is the highest, at `5`).
/// Amplifier `0` is the empty string: `getPotionDescription`'s `amplifier > 0`
/// gate means the numeral is omitted entirely, not rendered as `"I"`.
fn potency_numeral(amplifier: u8) -> &'static str {
    match amplifier {
        0 => "",
        1 => "II",
        2 => "III",
        3 => "IV",
        4 => "V",
        5 => "VI",
        _ => "",
    }
}

/// `Enchantment.getFullname` (`.cache/mc/26.2/src/net/minecraft/world/item
/// /enchantment/Enchantment.java`) for a stack carrying an
/// [`lodestone_model::AuthoredEnchantment`] — currently only a creative-menu
/// enchanted-book entry (see [`super::creative::stack_of`]'s own doc for why
/// this needs no live session data: the identity and level are known statically).
/// Empty for every other stack, and for one whose path this build's 43-entry
/// census does not recognise.
///
/// Two clauses, straight from the method:
///
/// 1. `Component.translatable(descriptionId)`, styled `RED` if the enchantment
///    carries `#minecraft:curse`, `GRAY` otherwise
///    ([`lodestone_data::enchantment::is_curse`]).
/// 2. A numeral suffix (`enchantment.level.<n>`), appended **unless**
///    `level == 1 && getMaxLevel() == 1` — i.e. omitted only for a single-level
///    enchantment shown at its (only) level `1`. A multi-level enchantment shown
///    at level `1` still gets `"I"`; this build's creative table only ever shows
///    the *max* level, so in practice this arm distinguishes a max-level-`1`
///    enchantment (no numeral) from every other (numeral shown).
fn enchantment_lore_lines(stack: &ItemStack) -> Vec<TooltipLine> {
    let Some(authored) = stack.authored_enchantment() else {
        return Vec::new();
    };
    let Some(name) = lodestone_data::enchantment::display_name(authored.path) else {
        return Vec::new();
    };
    let max_level = lodestone_data::enchantment::max_level(authored.path).unwrap_or(authored.level);
    let mut text = name.to_string();
    if authored.level != 1 || max_level != 1 {
        text = format!("{text} {}", enchantment_level_numeral(authored.level));
    }
    vec![TooltipLine {
        text,
        colour: if lodestone_data::enchantment::is_curse(authored.path) { RED } else { GRAY },
    }]
}

/// `en_us.json`'s `enchantment.level.<n>` table, `1..=10` — this build's
/// 43-entry census tops out at `5` (five enchantments share that max), so `6..=10`
/// are implemented but currently unreachable from real data, not guessed.
fn enchantment_level_numeral(level: u8) -> &'static str {
    match level {
        1 => "I",
        2 => "II",
        3 => "III",
        4 => "IV",
        5 => "V",
        6 => "VI",
        7 => "VII",
        8 => "VIII",
        9 => "IX",
        10 => "X",
        _ => "",
    }
}

/// `MobEffectUtil.formatDuration` / `StringUtil.formatTickDuration`
/// (`.cache/mc/26.2/src/net/minecraft/util/StringUtil.java`), with `scale = 1.0`
/// (this build models no `minecraft:potion_duration_scale` component) and
/// `tickrate = 20.0` (this build does not model a variable tick rate for tooltip
/// purposes). `%02d:%02d` (minutes, seconds) below one hour — no potion in this
/// build's registry reaches the `%02d:%02d:%02d` hour-carrying branch, but it is
/// still implemented, since the input is an arbitrary tick count, not one of the
/// 46 fixed durations.
fn format_duration_mmss(duration_ticks: u32) -> String {
    let total_seconds = duration_ticks / 20;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// `ItemAttributeModifiers.ATTRIBUTE_MODIFIER_FORMAT`: `new DecimalFormat("#.##")`
/// — up to two fraction digits, trailing zeros (and a bare trailing `.`) trimmed.
/// `raw_amount` is already sign-stripped by the caller; `percent` scales by `100`
/// first, matching `PotionContents.addPotionTooltip`'s
/// `modifier.operation() != ADD_MULTIPLIED_BASE/TOTAL ? amount : amount * 100.0`.
fn format_attribute_amount(raw_amount: f64, percent: bool) -> String {
    let display = if percent { raw_amount * 100.0 } else { raw_amount };
    let rounded = (display * 100.0).round() / 100.0;
    let mut formatted = format!("{rounded:.2}");
    while formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
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
/// * **over a slot** — `hovered`, resolved **once** by `build_inner` and shared
///   with the highlight sprites. This function used to run its own
///   `hit_test_with_book`; sharing the caller's answer is what makes "the tooltip
///   describes the slot the highlight is on, which is the slot a click would act
///   on" true by construction. It also means the overlay suppression
///   (`ContainerFrame::hover_blocked`) reaches the tooltip for free rather than
///   needing a second gate here.
/// * **slot holds a stack** — vanilla's
///   `if (hoveredSlot != null && hoveredSlot.hasItem())`
///   (`AbstractContainerScreen.java`).
/// * **nothing carried** — the same line's `&& carried.isEmpty()`. This is also
///   what makes the submission-order layering sound: the tooltip is emitted after
///   the carried stack, so if both could show at once the tooltip would cover it.
/// * **font** — with no `VanillaFont` there is nothing to measure the box against,
///   and a box sized off the 5×7 debug font would be visibly wrong. A jar-less run
///   draws no tooltip rather than a mis-sized one.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_tooltip(
    b: &mut Builder<'_>,
    menu: &Menu,
    hovered: Option<usize>,
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
    let Some(index) = hovered else { return };
    let Some(stack) = menu.slot_item(index) else {
        return;
    };
    emit_tooltip_for_stack(b, stack, cursor, advanced, gui_scale, width, height, canvas);
}

/// The box-and-lines half of [`emit_tooltip`], for a caller that already knows which
/// stack is hovered.
///
/// Split out for the creative screen, whose item list is a client-only container with
/// no [`Menu`] behind it — so it can name the stack but cannot name a menu slot. Every
/// guard [`emit_tooltip`] applies that is *about a menu* (nothing carried, a slot is
/// hovered, the slot holds a stack) stays with that function; everything below is
/// about the box.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_tooltip_for_stack(
    b: &mut Builder<'_>,
    stack: &ItemStack,
    cursor: Option<[f32; 2]>,
    advanced: bool,
    gui_scale: u32,
    width: u32,
    height: u32,
    canvas: (f32, f32),
) {
    let Some([cx, cy]) = cursor else { return };
    let Some(font) = b.font else { return };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn potion_stack(item_path: &str, potion_key: &str) -> ItemStack {
        let potion_id = lodestone_data::potion::potion_id(&format!("minecraft:{potion_key}")).expect("known potion");
        let mut stack = ItemStack::new(format!("minecraft:{item_path}").parse().expect("valid id"), 1);
        stack.set_potion_effect_id(Some(potion_id));
        stack
    }

    /// The two discriminating potions: `swiftness` (a percentage attribute
    /// modifier, no numeral, a duration whose minutes need no zero-pad) and
    /// `strong_turtle_master` (two effects, two different double-digit-or-not
    /// amplifiers, a flat-not-percentage-modified effect alongside one that is,
    /// and a duration under a minute). Expected text computed independently from
    /// `Potions.java`'s own constants (see this crate's `lodestone-data` tests
    /// for the numeric derivation), not from this module's own formatter.
    #[test]
    fn composed_strings_match_the_two_discriminating_potions() {
        let swiftness = tooltip_lines(&potion_stack("potion", "swiftness"), false);
        let turtle = tooltip_lines(&potion_stack("lingering_potion", "strong_turtle_master"), false);

        let mut mismatches = Vec::new();
        let expected_swiftness = [
            "Potion of Swiftness",
            "Speed (03:00)",
            "",
            "When Applied:",
            "+20% Speed",
        ];
        let got_swiftness: Vec<&str> = swiftness.iter().map(|l| l.text.as_str()).collect();
        if got_swiftness != expected_swiftness.to_vec() {
            mismatches.push(format!("swiftness: expected {expected_swiftness:?}, got {got_swiftness:?}"));
        }

        // `strong_turtle_master`'s `slowness` component (amplifier 5) carries an
        // attribute modifier of its own (`Speed`, base `-0.15`, `* (5 + 1) = -0.9`
        // -> `-90%`); its `resistance` component carries none. Getting this wrong
        // was the first draft of this test — the wrong hypothesis ("turtle_master
        // has no attribute-modifier section") looked plausible and was not true.
        let expected_turtle = [
            "Lingering Potion of the Turtle Master",
            "Slowness VI (00:20)",
            "Resistance IV (00:20)",
            "",
            "When Applied:",
            "-90% Speed",
        ];
        let got_turtle: Vec<&str> = turtle.iter().map(|l| l.text.as_str()).collect();
        if got_turtle != expected_turtle.to_vec() {
            mismatches.push(format!("strong_turtle_master: expected {expected_turtle:?}, got {got_turtle:?}"));
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// Amplifier `0` (`swiftness`) must render no numeral at all; amplifier `1`
    /// (`strong_swiftness`) must render `"II"` — a fixture using only one of the
    /// two cannot see a formatter that always renders (or never renders) a
    /// numeral. Same effect both times, so amplifier is the only variable.
    #[test]
    fn amplifier_zero_renders_no_numeral_amplifier_one_renders_ii() {
        let base = tooltip_lines(&potion_stack("potion", "swiftness"), false);
        let strong = tooltip_lines(&potion_stack("potion", "strong_swiftness"), false);
        assert_eq!(base[1].text, "Speed (03:00)");
        assert_eq!(strong[1].text, "Speed II (01:30)");
    }

    /// The duration formatter's own zero-pad control, independent of any real
    /// potion: a sub-ten-second value is the input that catches a missing
    /// leading zero on the seconds field (`"0:2"` vs `"00:02"`), and a
    /// single-digit minute count catches the same bug on minutes. `45` ticks is
    /// `floor(45 / 20) = 2` seconds — computed here, not read off a potion table.
    #[test]
    fn duration_formatter_zero_pads_sub_ten_second_and_single_digit_minute_values() {
        assert_eq!(format_duration_mmss(45), "00:02");
        assert_eq!(format_duration_mmss(20 * 65), "01:05");
    }

    /// The water-bottle control, for the *lore*, not the colour: no
    /// `MobEffectInstance` at all, so vanilla's `noEffects` branch fires and
    /// prints its own `"No Effects"` line — this must not be an empty lore
    /// section, proving the empty-effects case is handled deliberately.
    #[test]
    fn water_bottle_prints_no_effects_not_an_empty_section() {
        let lines = tooltip_lines(&potion_stack("potion", "water"), false);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Water Bottle");
        assert_eq!(lines[1].text, "No Effects");
        assert_eq!(lines[1].colour, GRAY);
    }

    /// An instant effect's duration (`healing`, `1` tick) falls inside
    /// `endsWithin(20)` and must print no duration suffix at all — `swiftness`
    /// alone (`3600` ticks) cannot see a formatter that always appends one.
    #[test]
    fn instant_effect_prints_no_duration_suffix() {
        let lines = tooltip_lines(&potion_stack("potion", "healing"), false);
        assert_eq!(lines[1].text, "Instant Health");
    }

    /// `night_vision` carries a real effect but no attribute modifier — the
    /// control that separates "no attribute-modifier section" from "no effects
    /// at all". Must be exactly one lore line, with no blank/"When Applied:"
    /// tail.
    #[test]
    fn effect_with_no_attribute_modifier_prints_no_when_applied_section() {
        let lines = tooltip_lines(&potion_stack("potion", "night_vision"), false);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "Night Vision (03:00)");
    }

    /// A custom name overrides the composed potion title outright — vanilla's
    /// `ItemStack.getHoverName()` checks `DataComponents.CUSTOM_NAME` before
    /// `Item.getName` ever runs, so `PotionItem`/`TippedArrowItem`'s override
    /// never gets a chance. Checked as a differential against
    /// [`lodestone_game::item::styled_hover_name`] directly, and as a negative
    /// against the potion title, rather than duplicating the italic-formatting
    /// logic here.
    #[test]
    fn custom_name_overrides_the_composed_potion_title() {
        let mut stack = potion_stack("potion", "swiftness");
        stack.set_custom_name(Some(lodestone_model::Text::literal("Elixir of Life")));

        let title = hover_name(&stack);
        assert_eq!(title, lodestone_game::item::styled_hover_name(&stack, &|_| None));
        assert_ne!(title, "Potion of Swiftness");
    }

    /// The neuter this module's own doc guards against: if the potion title
    /// special-case were deleted, `hover_name` would fall straight through to
    /// `styled_hover_name`'s generic humanised fallback, which reads the bare
    /// item id and knows nothing about the potion — `"Potion"`, not
    /// `"Potion of Swiftness"`. This is the assertion that would catch it.
    #[test]
    fn potion_title_is_not_the_generic_humanised_item_name() {
        let stack = potion_stack("potion", "swiftness");
        assert_ne!(hover_name(&stack), "Potion");
    }

    fn enchanted_book_stack(path: &str) -> ItemStack {
        let level = lodestone_data::enchantment::max_level(path).expect("known enchantment");
        let mut stack = ItemStack::new("minecraft:enchanted_book".parse().unwrap(), 1);
        stack.set_authored_enchantment(Some(lodestone_model::AuthoredEnchantment {
            path: lodestone_data::enchantment::canonical_path(path).unwrap(),
            level,
        }));
        stack
    }

    /// The two discriminating enchantments the coordinator's brief asked for:
    /// `mending` (max level `1`, so its own max-level lore renders **no**
    /// numeral) and `sharpness` (max level `5`, so it renders `"V"`) — a fixture
    /// using only one of the two cannot see a formatter that always renders (or
    /// never renders) a level suffix. The title stays the item's generic
    /// `"Enchanted Book"` either way — vanilla has no `EnchantedBookItem.getName`
    /// override; only the lore differs, unlike a potion's title.
    #[test]
    fn composed_lore_distinguishes_a_single_level_enchantment_from_a_multi_level_one() {
        let mending = tooltip_lines(&enchanted_book_stack("mending"), false);
        let sharpness = tooltip_lines(&enchanted_book_stack("sharpness"), false);

        let mut mismatches = Vec::new();
        if mending.iter().map(|l| l.text.as_str()).collect::<Vec<_>>() != vec!["Enchanted Book", "Mending"] {
            mismatches.push(format!("mending: {mending:?}"));
        }
        if sharpness.iter().map(|l| l.text.as_str()).collect::<Vec<_>>() != vec!["Enchanted Book", "Sharpness V"] {
            mismatches.push(format!("sharpness: {sharpness:?}"));
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// A cursed enchantment's lore line is red, not gray, and its name reorders
    /// the words entirely (`"Curse of Binding"`, not `"Binding Curse"`) — the
    /// irregular-name control for enchantments, matching the potion module's
    /// `turtle_master` case.
    #[test]
    fn curse_enchantment_lore_is_red_with_the_reordered_name() {
        let lines = tooltip_lines(&enchanted_book_stack("binding_curse"), false);
        assert_eq!(lines[1].text, "Curse of Binding");
        assert_eq!(lines[1].colour, RED);
    }

    /// The control that separates "no `authored_enchantment`" from "an unnamed
    /// one": a plain enchanted-book stack with nothing set must show no lore line
    /// at all, and must not be confused with the real
    /// `enchantments()`/`ENCHANTMENTS_COMPONENT` list, which this stack also
    /// carries none of.
    #[test]
    fn a_book_with_no_authored_enchantment_has_no_enchantment_lore() {
        let stack = ItemStack::new("minecraft:enchanted_book".parse().unwrap(), 1);
        let lines = tooltip_lines(&stack, false);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Enchanted Book");
    }
}
