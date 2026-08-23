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
//! * **Authored `minecraft:lore` is styled through a parent.** Vanilla supplies
//!   dark-purple italics as defaults, but an authored child can explicitly turn
//!   italics off or select an RGB colour. [`lore_lines`] wraps the decoded tree
//!   instead of overwriting its root style, so ordinary text inherits the
//!   defaults and explicit child formatting still wins. Enchantment lines are
//!   absent for a different reason: `ItemEnchantment::id`
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
use lodestone_model::text::{Text, TextColor, TextSpan, TextStyle};

use crate::hud::VanillaFont;

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

/// `ClientBundleTooltip.GRID_WIDTH`/`getWidth` (`ClientBundleTooltip.java`) —
/// the bundle image component's own fixed width, centred within a wider box
/// exactly like [`title_line`]'s text is left-aligned within it.
const BUNDLE_GRID_W: f32 = 96.0;
/// `ClientBundleTooltip.SLOT_SIZE`.
const BUNDLE_SLOT: f32 = 24.0;
/// `ClientBundleTooltip.SLOT_MARGIN` — the icon's inset within its slot cell.
const BUNDLE_SLOT_MARGIN: f32 = 4.0;
/// `ClientBundleTooltip.PROGRESSBAR_HEIGHT`/`_WIDTH`.
const BUNDLE_PROGRESSBAR_H: f32 = 13.0;
const BUNDLE_PROGRESSBAR_W: f32 = 96.0;
/// `ClientBundleTooltip.PROGRESSBAR_FILL_MAX` — the fill's own inner span,
/// one pixel shy of the border on each side.
const BUNDLE_PROGRESSBAR_FILL_MAX: f32 = 94.0;
/// The vertical gap [`bundle_image_height`] spends twice — once between the
/// grid/description and the bar (`PROGRESSBAR_MARGIN_Y`), once below the bar
/// closing out the component — `backgroundHeight() = itemGridHeight() + 13 +
/// 8`, where the trailing `8` is this constant counted twice.
const BUNDLE_BOTTOM_PAD: f32 = 4.0;
/// A bundle slot cell's plain (unselected) fill — this module's own flat
/// stand-in for `container/bundle/slot_background`, the same simplification
/// [`TOOLTIP_BG`]'s doc explains for the main box: the sprite atlas is out of
/// reach from this stream (see this module's doc), so a flat colour close to
/// vanilla's real sprite art draws instead.
const BUNDLE_SLOT_BG: [f32; 4] = [1.0, 1.0, 1.0, 0.15];
/// The selected slot's back fill — this module's stand-in for
/// `container/bundle/slot_highlight_back`, noticeably brighter than
/// [`BUNDLE_SLOT_BG`] so the highlight the scroll wiring produces is
/// actually visible, which is the entire point of this module existing.
const BUNDLE_SLOT_HIGHLIGHT: [f32; 4] = [1.0, 1.0, 1.0, 0.4];
/// The selected slot's one-pixel front border — this module's stand-in for
/// `container/bundle/slot_highlight_front`, drawn over the icon the same way
/// the real sprite is (`extractSlot`'s own back-then-icon-then-front order).
const BUNDLE_SLOT_HIGHLIGHT_BORDER: [f32; 4] = [1.0, 1.0, 1.0, 0.9];
/// The progress bar's empty track — this module's stand-in for
/// `container/bundle/slot_background`'s progressbar sibling.
const BUNDLE_PROGRESSBAR_BG: [f32; 4] = [1.0, 1.0, 1.0, 0.15];
/// The progress bar's fill — a plain, readable green rather than vanilla's
/// own two-tone sprite gradient.
const BUNDLE_PROGRESSBAR_FILL: [f32; 4] = [80.0 / 255.0, 200.0 / 255.0, 120.0 / 255.0, 1.0];
/// `item.minecraft.bundle.empty.description` (`en_us.json`) — the same
/// "no language table reaches this module" simplification [`tooltip_lines`]'s
/// own doc already carries for `item.durability`/`item.components`.
const BUNDLE_EMPTY_DESCRIPTION: &str = "Can hold a mixed stack of items";

/// One tooltip line: its text and its colour.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct TooltipLine {
    /// The text, already resolved to words. Still populated when
    /// [`Self::spans`] is `Some` (as the plain concatenation) so width
    /// measurement and any caller that only wants the words keep working
    /// unchanged; the draw loop prefers `spans` when present.
    pub text: String,
    /// The colour to draw it in when [`Self::spans`] is `None` — every line
    /// except the title, which is a fixed English literal or a potion name
    /// composed by this module, never a server-authored [`Text`] tree.
    pub colour: [f32; 4],
    /// The styled runs behind `text`, when the line was built from a real
    /// [`Text`] tree that might carry a hex colour — currently only the
    /// title and authored lore lines. `None` for composed client-owned lines,
    /// which draw flat in `colour` instead.
    pub spans: Option<Vec<TextSpan>>,
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
    let mut lines = vec![title_line(stack)];
    lines.extend(lore_lines(stack));
    // `PotionContents.addToTooltip`/`ItemEnchantments.addToTooltip` both run
    // unconditionally — neither is gated behind `isAdvanced()` the way
    // durability/id/component-count are, so these lines belong before the
    // `advanced` early-return, not after it. A stack is never both, so at most
    // one of the two `extend`s below is ever non-empty.
    lines.extend(potion_lore_lines(stack));
    lines.extend(enchantment_lore_lines(stack));
    lines.extend(book_lore_lines(stack));
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
            spans: None,
        });
    }
    lines.push(TooltipLine {
        text: stack.item().to_string(),
        colour: DARK_GRAY,
        spans: None,
    });
    let components = stack.components().len();
    if components > 0 {
        lines.push(TooltipLine {
            text: format!("{components} Component(s)"),
            colour: DARK_GRAY,
            spans: None,
        });
    }
    lines
}

/// The authored `minecraft:lore` body immediately below the item name.
///
/// `ItemLore` applies dark-purple italics as a parent style. Building a wrapper
/// node is load-bearing: mutating the authored root would replace rather than
/// inherit formatting, so an explicit child `italic: false` or RGB colour could
/// no longer override the default.
fn lore_lines(stack: &ItemStack) -> Vec<TooltipLine> {
    stack
        .lore()
        .iter()
        .map(|line| {
            let styled = Text {
                style: TextStyle {
                    color: Some(TextColor::DarkPurple),
                    italic: Some(true),
                    ..TextStyle::default()
                },
                extra: vec![line.clone()],
                ..Text::default()
            };
            TooltipLine {
                text: line.to_plain_string(),
                colour: DARK_PURPLE,
                spans: Some(styled.to_spans()),
            }
        })
        .collect()
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
///
/// Returns the full [`TooltipLine`] rather than a bare `String` so the
/// [`styled_hover_name_spans`](lodestone_game::item::styled_hover_name_spans)
/// case can carry its spans alongside the plain text: `text` still comes from
/// [`styled_hover_name`](lodestone_game::item::styled_hover_name) (so
/// `emit_tooltip_for_stack`'s box-width walk, which measures every line
/// through the same `§`-aware `font.width`, is unaffected), and `spans` is
/// `Some` only when the title actually came from a `Text` tree that might
/// carry a hex colour — never for the composed potion title, which this
/// module builds as a plain literal and can never disagree with itself on
/// colour.
fn title_line(stack: &ItemStack) -> TooltipLine {
    if stack.custom_name().is_none()
        && let Some(potion_id) = stack.potion_effect_id()
        && let Some(name) = lodestone_data::potion::potion_item_display_name(stack.item().path(), potion_id)
    {
        return TooltipLine {
            text: name.to_string(),
            colour: NAME_COLOUR,
            spans: None,
        };
    }
    // `&|_| None`: no language table here, so a non-potion item resolves through
    // `base_display_name`'s humanised fallback — "Diamond Sword" for
    // `minecraft:diamond_sword`. Right for every vanilla item whose id is its
    // name in snake_case, which is nearly all of them.
    TooltipLine {
        text: lodestone_game::item::styled_hover_name(stack, &|_| None),
        colour: NAME_COLOUR,
        spans: Some(lodestone_game::item::styled_hover_name_spans(stack, &|_| None)),
    }
}

/// Test/back-compat accessor: the title line's plain text alone, matching
/// [`title_line`]'s `text` field exactly (see that function's own doc for why
/// the two never disagree on wording, only on whether spans ride along).
#[cfg(test)]
fn hover_name(stack: &ItemStack) -> String {
    title_line(stack).text
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
            spans: None,
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
            spans: None,
        });
    }
    let modifiers = lodestone_data::potion::potion_attribute_modifiers(potion_id);
    if !modifiers.is_empty() {
        lines.push(TooltipLine {
            text: String::new(),
            colour: NAME_COLOUR,
            spans: None,
        });
        lines.push(TooltipLine {
            text: "When Applied:".to_string(),
            colour: DARK_PURPLE,
            spans: None,
        });
        for modifier in &modifiers {
            let magnitude = format_attribute_amount(modifier.amount.abs(), modifier.percent);
            let sign = if modifier.amount < 0.0 { "-" } else { "+" };
            let suffix = if modifier.percent { "%" } else { "" };
            lines.push(TooltipLine {
                text: format!("{sign}{magnitude}{suffix} {}", modifier.attribute_name),
                colour: if modifier.amount < 0.0 { RED } else { BLUE },
                spans: None,
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

/// `WrittenBookContent.addToTooltip` for a signed
/// `minecraft:written_book` — the author and copy-generation lines. Empty for
/// every other stack, including a `minecraft:writable_book`
/// (`WritableBookContent` is not a `TooltipProvider` at all: an unsigned book
/// has no author and no generation).
///
/// Two lines, in the method's own order, both `ChatFormatting.GRAY`:
///
/// 1. `book.byAuthor` — `"by %1$s"` — **skipped when the author is blank**
///    (`StringUtil.isBlank`), which is what an unattributed `/give`-built
///    book carries.
/// 2. `book.generation.<n>` — `"Original"` / `"Copy of original"` /
///    `"Copy of a copy"` / `"Tattered"`, unconditional. Vanilla's own
///    `WrittenBookContent` constructor rejects anything outside `0..=3`, and
///    [`lodestone_model::WrittenBookContent::generation`] is a `u8`, so an
///    out-of-range value can only reach here from a non-vanilla server; it
///    falls back to `"Original"` rather than printing a raw key.
///
/// The strings are English literals for the same reason every other line in
/// this module is one — no language table reaches here; see
/// [`tooltip_lines`]'s own note.
///
/// The book's **title** is not a line here: it is the item's display *name*,
/// resolved by `lodestone_game::item::styled_hover_name` through vanilla's
/// `ItemStack.getCustomName()` fallback, so it is already the tooltip's first
/// line via [`title_line`].
fn book_lore_lines(stack: &ItemStack) -> Vec<TooltipLine> {
    let Some(content) = stack.written_book_content() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let author = content.author.trim();
    if !author.is_empty() {
        lines.push(TooltipLine {
            text: format!("by {author}"),
            colour: GRAY,
            spans: None,
        });
    }
    lines.push(TooltipLine {
        text: book_generation_name(content.generation).to_string(),
        colour: GRAY,
        spans: None,
    });
    lines
}

/// `book.generation.<n>`'s four `en_us.json` strings, in
/// [`lodestone_model::WrittenBookContent::generation`]'s own order.
///
/// Shared with the book-view screen's header
/// (`crate::menu::book_view`), which draws the identical
/// line — one table, so the tooltip and the open book can never disagree
/// about what a copy of a copy is called.
#[must_use]
pub(crate) fn book_generation_name(generation: u8) -> &'static str {
    match generation {
        1 => "Copy of original",
        2 => "Copy of a copy",
        3 => "Tattered",
        _ => "Original",
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
        spans: None,
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
    assets: &crate::hud::item_icon::IconAssets<'_>,
    menu: &Menu,
    hovered: Option<usize>,
    cursor: Option<[f32; 2]>,
    advanced: bool,
    gui_scale: u32,
    width: u32,
    height: u32,
    canvas: (f32, f32),
    bundle_selection: Option<super::bundle::BundleSelection>,
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
    // Only a selection tracked against *this* hovered slot applies — see
    // `crate::container::bundle`'s module doc for why the tracked selection
    // lives beside the menu rather than mutated into the stack itself, and
    // `ContainerFrame::bundle_selection`'s own doc for why the window-id
    // filter happens one layer up, at the render call site.
    #[allow(clippy::cast_possible_wrap)]
    let selected = bundle_selection
        .filter(|s| s.slot == index as i32)
        .map(|s| s.selected);
    emit_tooltip_for_stack(
        b, assets, stack, cursor, advanced, gui_scale, width, height, canvas, selected,
    );
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
    assets: &crate::hud::item_icon::IconAssets<'_>,
    stack: &ItemStack,
    cursor: Option<[f32; 2]>,
    advanced: bool,
    gui_scale: u32,
    width: u32,
    height: u32,
    canvas: (f32, f32),
    bundle_selected: Option<i32>,
) {
    let Some([cx, cy]) = cursor else { return };
    let Some(font) = b.font else { return };
    let lines = tooltip_lines(stack, advanced);
    if lines.is_empty() {
        return;
    }

    // `ItemStack.getTooltipImage`/`BundleItem.getTooltipImage`: a bundle's own
    // tooltip carries an extra "image" component (the item grid), inserted
    // right after the title line — see `bundle_image_height`'s own doc.
    let bundle_image_h =
        lodestone_game::item::is_bundle(stack.item()).then(|| bundle_image_height(font, stack));

    // `cursor` is physical viewport space (`hit_test`'s), this builder is the
    // logical canvas — the same division the carried-stack draw performs, and for
    // the same reason.
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let (cx, cy) = (cx / scale, cy / scale);

    let mut text_w = lines
        .iter()
        .map(|l| font.width(&l.text, 1.0))
        .fold(0.0f32, f32::max);
    // The image component (currently only a bundle's grid) is centred within
    // the box rather than left-aligned like every text line — `ClientBundleTooltip
    // .getWidth` is a fixed `96`, so a box narrower than that (a short title,
    // no advanced lines) must still grow to fit it.
    if bundle_image_h.is_some() {
        text_w = text_w.max(BUNDLE_GRID_W);
    }
    // The height walk: one `LINE_H` per line at `LINE_PITCH`, plus vanilla's extra
    // 2 px under the title when there is a body — and the image component's own
    // height, inserted right after the title (see `bundle_image_h`'s own doc
    // above). The `lines.len() == 1` arm needs its own gap added, since the base
    // formula only adds `TITLE_GAP` when a *text* body follows; an image-only
    // tail still needs separation from the title.
    let mut text_h = if lines.len() > 1 {
        LINE_H + TITLE_GAP + (lines.len() - 1) as f32 * LINE_PITCH
    } else {
        LINE_H
    };
    if let Some(image_h) = bundle_image_h {
        if lines.len() == 1 {
            text_h += TITLE_GAP;
        }
        text_h += image_h;
    }

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
        // `spans` is `Some` only for a title built from a real `Text` tree
        // (see `title_line`'s doc) — draw those through the span-aware path
        // so a hex-coloured custom item name survives; every other line is a
        // plain literal this module composed itself and draws flat as before.
        match &line.spans {
            Some(spans) => b.shadowed_label_spans(spans, tx, y, 1.0, line.colour),
            None => b.shadowed_label(&line.text, tx, y, 1.0, line.colour),
        }
        y += LINE_PITCH;
        if i == 0 {
            y += TITLE_GAP;
            // The image component, right after the title — see
            // `ItemStack.getTooltipImage`'s own insertion point
            // (`components.add(components.isEmpty() ? 0 : 1, …)`), and
            // `bundle_image_h`'s doc above for why this is the only image
            // kind implemented.
            if let Some(image_h) = bundle_image_h {
                draw_bundle_image(b, assets, font, stack, tx, y, text_w, bundle_selected);
                y += image_h;
            }
        }
    }
}

/// How many rows [`draw_bundle_image`]'s grid needs — `Mth.positiveCeilDiv
/// (slotCount, 4)` where `slotCount = min(12, contents.len())`
/// (`ClientBundleTooltip.gridSizeY`/`slotCount`). Only meaningful for a
/// non-empty bundle; the empty case has its own layout entirely (see
/// [`bundle_image_height`]).
fn bundle_grid_rows(len: usize) -> usize {
    len.min(12).div_ceil(4).max(1)
}

/// The bundle tooltip's own image-component height —
/// `ClientBundleTooltip.getHeight`/`backgroundHeight`/
/// `getEmptyBundleBackgroundHeight`, ported directly: an empty bundle shows
/// wrapped description text over the (always-empty) progress bar; a
/// non-empty one shows the item grid over the (weight-filled) bar. Both tack
/// on the same `13 + 8` tail — the bar's own height plus
/// [`BUNDLE_BOTTOM_PAD`] spent twice, once above the bar and once below it.
fn bundle_image_height(font: &VanillaFont, stack: &ItemStack) -> f32 {
    let content_h = if stack.bundle_contents().is_empty() {
        wrap_bundle_description(font).len() as f32 * LINE_H
    } else {
        bundle_grid_rows(stack.bundle_contents().len()) as f32 * BUNDLE_SLOT
    };
    content_h + BUNDLE_PROGRESSBAR_H + BUNDLE_BOTTOM_PAD * 2.0
}

/// A plain greedy word-wrap of [`BUNDLE_EMPTY_DESCRIPTION`] against
/// [`BUNDLE_GRID_W`] — the same reduction of vanilla's `StringSplitter
/// ::splitLines` `crate::menu::advancements::wrap` already uses for a single
/// unstyled run, kept as a private copy here rather than made `pub(crate)`
/// there: the two live in unrelated subsystems and a shared helper would be
/// a coupling with no other caller to justify it.
fn wrap_bundle_description(font: &VanillaFont) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in BUNDLE_EMPTY_DESCRIPTION.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if font.width(&candidate, 1.0) <= BUNDLE_GRID_W || current.is_empty() {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// `BundleContents.getWeight`/`computeContentWeight`/`getWeight`
/// (`BundleContents.java`) as an `f32` rather than an exact `Fraction` — this
/// build has no rational type, and the progress bar only ever quantises the
/// result to [`BUNDLE_PROGRESSBAR_FILL_MAX`] steps, so a float loses nothing
/// the sprite could show. Recurses for a bundle nested inside a bundle
/// (`BUNDLE_IN_BUNDLE_WEIGHT = 1/16`, added on top of the nested bundle's own
/// weight) — the recursive-decode chain issue #692's audit found genuinely
/// complete (see this module's caller for the pointer). A beehive's
/// `minecraft:bees` clause (`getWeight`'s `BEEHIVE_WEIGHT = 1`) is not
/// modelled: this build has no `Bees` component anywhere, so a bee nest
/// dropped into a bundle falls through to the ordinary `1 / max_stack_size`
/// term below instead of the real flat `1` — a documented, narrow gap rather
/// than a guess.
fn bundle_weight(stack: &ItemStack) -> f32 {
    stack
        .bundle_contents()
        .iter()
        .map(|item| {
            let per_item = if lodestone_game::item::is_bundle(item.item()) {
                bundle_weight(item) + (1.0 / 16.0)
            } else {
                // `max_stack_size` is already clamped to `1..=99`, so this
                // can never divide by zero.
                1.0 / item.max_stack_size() as f32
            };
            per_item * item.count().max(0) as f32
        })
        .sum()
}

/// The progress bar — `ClientBundleTooltip::extractProgressbar`, as a flat
/// fill over a flat track rather than the two real sprites (see
/// [`BUNDLE_PROGRESSBAR_BG`]'s own doc). `weight` is [`bundle_weight`]'s
/// output, already clamped to `[0, 1]` by the caller for the fill width but
/// read unclamped here for the full/empty text choice, matching
/// `getProgressBarFillText`'s own `Fraction` comparisons.
fn draw_bundle_progressbar(b: &mut Builder<'_>, font: &VanillaFont, x: f32, y: f32, weight: f32) {
    b.rect_px(x, y, BUNDLE_PROGRESSBAR_W, BUNDLE_PROGRESSBAR_H, BUNDLE_PROGRESSBAR_BG);
    let fill_w = (weight.clamp(0.0, 1.0) * BUNDLE_PROGRESSBAR_FILL_MAX).round();
    if fill_w > 0.0 {
        b.rect_px(x + 1.0, y + 1.0, fill_w, BUNDLE_PROGRESSBAR_H - 2.0, BUNDLE_PROGRESSBAR_FILL);
    }
    b.rect_px(x, y, BUNDLE_PROGRESSBAR_W, 1.0, BORDER_TOP);
    b.rect_px(x, y + BUNDLE_PROGRESSBAR_H - 1.0, BUNDLE_PROGRESSBAR_W, 1.0, BORDER_BOTTOM);
    let text = if weight <= 0.0 {
        Some("Empty")
    } else if weight >= 1.0 {
        Some("Full")
    } else {
        None
    };
    if let Some(text) = text {
        let tw = font.width(text, 1.0);
        b.shadowed_label(
            text,
            x + BUNDLE_PROGRESSBAR_W / 2.0 - tw / 2.0,
            y + 3.0,
            1.0,
            NAME_COLOUR,
        );
    }
}

/// The bundle tooltip's own image component —
/// `ClientBundleTooltip::extractImage`/`extractBundleWithItemsTooltip`/
/// `extractEmptyBundleTooltip`, ported directly including the item grid's
/// bottom-to-top, right-to-left fill order (`extractBundleWithItemsTooltip`'s
/// own `rowNumber`/`columnNumber` walk) and the `+N` overflow count in the
/// bottom-right cell when the bundle holds more than
/// [`lodestone_game::item::ItemStack::bundle_items_to_show`] can display.
///
/// `x` is the tooltip content box's own left edge (vanilla's `x` parameter to
/// `extractImage`, *not* pre-centred), `y` is this component's own top, and
/// `box_w` is the overall box's content width — the same three vanilla hands
/// `extractImage`. The grid itself is centred within `box_w` via
/// `getContentXOffset`; text lines elsewhere in this file stay left-aligned
/// at `x`, which is why centring happens here rather than by widening `x`
/// itself.
///
/// `selected` is the shown-item index — [`crate::container::bundle
/// ::BundleSelection::selected`]'s own index space, which is already the same
/// one `BundleContents.getSelectedItemIndex()`/`itemVisualOrderIndex` use (both
/// range over the *shown* subset, `0` = most recently inserted), so no
/// remapping happens here.
#[allow(clippy::too_many_arguments)]
fn draw_bundle_image(
    b: &mut Builder<'_>,
    assets: &crate::hud::item_icon::IconAssets<'_>,
    font: &VanillaFont,
    stack: &ItemStack,
    x: f32,
    y: f32,
    box_w: f32,
    selected: Option<i32>,
) {
    let content_x = x + (box_w - BUNDLE_GRID_W) / 2.0;
    let contents = stack.bundle_contents();
    if contents.is_empty() {
        // `extractEmptyBundleDescriptionText`'s own colour, `-5592406` —
        // `0xFFAAAAAA`, i.e. [`GRAY`] at full alpha.
        let lines = wrap_bundle_description(font);
        let mut ly = y;
        for line in &lines {
            b.shadowed_label(line, content_x, ly, 1.0, GRAY);
            ly += LINE_H;
        }
        draw_bundle_progressbar(b, font, content_x, ly + BUNDLE_BOTTOM_PAD, 0.0);
        return;
    }

    let show = stack.bundle_items_to_show();
    let shown = &contents[..show.min(contents.len())];
    let rows = bundle_grid_rows(contents.len());
    let overflowing = contents.len() > 12;
    let x_start = content_x + BUNDLE_GRID_W;
    let y_start = y + rows as f32 * BUNDLE_SLOT;
    let mut slot_number = 1usize;
    for row in 1..=rows {
        for col in 1..=4u32 {
            let draw_x = x_start - col as f32 * BUNDLE_SLOT;
            let draw_y = y_start - row as f32 * BUNDLE_SLOT;
            if overflowing && row == 1 && col == 1 {
                // `extractCount`'s own anchor: centred text at `(drawX+12,
                // drawY+10)`.
                let hidden: i64 = contents.iter().skip(shown.len()).map(|s| i64::from(s.count())).sum();
                let text = format!("+{hidden}");
                let tw = font.width(&text, 1.0);
                b.shadowed_label(&text, draw_x + 12.0 - tw / 2.0, draw_y + 10.0, 1.0, NAME_COLOUR);
            } else if shown.len() >= slot_number {
                // `itemVisualOrderIndex = shownItems.size() - slotNumber`
                // (`extractSlot`), using the pre-increment `slotNumber`.
                let item_index = shown.len() - slot_number;
                let highlighted = selected == Some(item_index as i32);
                b.rect_px(
                    draw_x,
                    draw_y,
                    BUNDLE_SLOT,
                    BUNDLE_SLOT,
                    if highlighted { BUNDLE_SLOT_HIGHLIGHT } else { BUNDLE_SLOT_BG },
                );
                b.draw_stack(assets, &shown[item_index], draw_x + BUNDLE_SLOT_MARGIN, draw_y + BUNDLE_SLOT_MARGIN);
                if highlighted {
                    // The front highlight ring, over the icon — `extractSlot`'s
                    // own back-then-icon-then-front order.
                    b.rect_px(draw_x, draw_y, BUNDLE_SLOT, 1.0, BUNDLE_SLOT_HIGHLIGHT_BORDER);
                    b.rect_px(draw_x, draw_y + BUNDLE_SLOT - 1.0, BUNDLE_SLOT, 1.0, BUNDLE_SLOT_HIGHLIGHT_BORDER);
                    b.rect_px(draw_x, draw_y, 1.0, BUNDLE_SLOT, BUNDLE_SLOT_HIGHLIGHT_BORDER);
                    b.rect_px(draw_x + BUNDLE_SLOT - 1.0, draw_y, 1.0, BUNDLE_SLOT, BUNDLE_SLOT_HIGHLIGHT_BORDER);
                }
                slot_number += 1;
            }
        }
    }

    // The selected item's own floating name — `extractSelectedItemTooltip`,
    // the thing that makes the scroll-selection feature legible at a glance
    // rather than just a brighter square: `getStyledHoverName`'s own
    // `&|_| None` fallback matches [`title_line`]'s (no language table here).
    if let Some(item) = selected
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| shown.get(i))
    {
        let name = lodestone_game::item::styled_hover_name(item, &|_| None);
        let tw = font.width(&name, 1.0);
        let center = x + box_w / 2.0 - 12.0;
        let name_x = center - tw / 2.0;
        let name_y = y - 15.0;
        b.rect_px(name_x - PADDING, name_y - PADDING, tw + PADDING * 2.0, LINE_H + PADDING * 2.0, TOOLTIP_BG);
        b.shadowed_label(&name, name_x, name_y, 1.0, NAME_COLOUR);
    }

    draw_bundle_progressbar(b, font, content_x, y + rows as f32 * BUNDLE_SLOT + BUNDLE_BOTTOM_PAD, bundle_weight(stack));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Authored lore belongs directly below the title. Vanilla supplies a
    /// dark-purple italic parent style, while explicit formatting on the
    /// authored component remains authoritative through normal inheritance.
    #[test]
    fn authored_lore_renders_after_the_title_with_vanilla_defaults_and_child_overrides() {
        let mut line = lodestone_model::Text::literal("Ancient ");
        let mut child = lodestone_model::Text::literal("Rune");
        child.style.color = Some(lodestone_model::TextColor::Rgb(0x12_ab_ef));
        child.style.italic = Some(false);
        line.extra.push(child);

        let mut stack = ItemStack::new("minecraft:paper".parse().expect("valid id"), 1);
        stack.set_lore(vec![line]);
        let lines = tooltip_lines(&stack, false);

        assert_eq!(
            lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(),
            vec!["Paper", "Ancient Rune"]
        );
        let spans = lines[1].spans.as_ref().expect("lore keeps styled spans");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].style.color, Some(lodestone_model::TextColor::DarkPurple));
        assert_eq!(spans[0].style.italic, Some(true));
        assert_eq!(spans[1].style.color, Some(lodestone_model::TextColor::Rgb(0x12_ab_ef)));
        assert_eq!(spans[1].style.italic, Some(false));
    }

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

    fn id(s: &str) -> lodestone_model::Identifier {
        s.parse().expect("valid id")
    }

    fn bundle_of(items: Vec<ItemStack>) -> ItemStack {
        let mut stack = ItemStack::new(id("minecraft:bundle"), 1);
        stack.set_bundle_contents(items);
        stack
    }

    fn torches(count: usize) -> Vec<ItemStack> {
        (0..count).map(|_| ItemStack::new(id("minecraft:torch"), 1)).collect()
    }

    /// `BundleContents.getWeight` for a flat bundle: each torch (default max
    /// stack size 64, since [`ItemStack::new`] carries no
    /// `minecraft:max_stack_size` override) contributes `1/64`.
    #[test]
    fn bundle_weight_sums_reciprocal_max_stack_sizes() {
        let stack = bundle_of(torches(3));
        assert!(
            (bundle_weight(&stack) - 3.0 / 64.0).abs() < 1e-6,
            "got {}",
            bundle_weight(&stack)
        );
    }

    /// The recursive clause: a bundle nested inside a bundle contributes its
    /// own weight plus [`BUNDLE_IN_BUNDLE_WEIGHT`]'s `1/16` —
    /// `BundleContents::getWeight`'s `nestedWeight.add(BUNDLE_IN_BUNDLE_WEIGHT)`.
    /// This is also the discriminating case for issue #692's own claim that the
    /// recursive bundle-in-bundle decode is complete: a weight that only ever
    /// read the outer stack's own component would silently treat the inner
    /// bundle as weightless instead of `4/64 + 1/16`.
    #[test]
    fn bundle_weight_recurses_into_a_nested_bundle() {
        let inner = bundle_of(torches(4));
        let outer = bundle_of(vec![inner]);
        let expected = 4.0 / 64.0 + 1.0 / 16.0;
        assert!(
            (bundle_weight(&outer) - expected).abs() < 1e-6,
            "got {}, want {expected}",
            bundle_weight(&outer)
        );
    }

    /// [`bundle_grid_rows`] against the same four worked cases
    /// `ItemStack::bundle_items_to_show`'s own test already establishes for
    /// `getNumberOfItemsToShow` — `Mth.positiveCeilDiv(min(12, size), 4)`.
    #[test]
    fn bundle_grid_rows_matches_vanillas_worked_cases() {
        let cases = [(4, 1), (6, 2), (16, 3), (13, 3)];
        let mut mismatches = Vec::new();
        for (size, want) in cases {
            let got = bundle_grid_rows(size);
            if got != want {
                mismatches.push(format!("size {size}: got {got} rows, want {want}"));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// The whole point of this module: a tracked scroll selection must draw a
    /// visibly different fill on its own slot, and an *absent* selection must
    /// draw that fill nowhere — the control that proves the highlight isn't
    /// just always on. Jar-less-safe would need no font at all, but the box's
    /// own layout (and therefore where the grid lands) is measured through
    /// [`VanillaFont`], so this skips without one rather than asserting
    /// against the fixed-advance debug font's different metrics.
    #[test]
    fn a_selected_bundle_slot_draws_the_highlight_fill_and_an_absent_one_draws_none() {
        let Some(font) = VanillaFont::shared() else {
            return; // jar-less: nothing to measure against
        };
        let stack = bundle_of(vec![
            ItemStack::new(id("minecraft:torch"), 1),
            ItemStack::new(id("minecraft:torch"), 1),
            ItemStack::new(id("minecraft:stick"), 1),
        ]);
        let assets = crate::hud::item_icon::IconAssets { items: None, models: None };
        let has_alpha = |verts: &[f32], alpha: f32| {
            verts.chunks_exact(6).any(|v| (v[5] - alpha).abs() < 1e-4)
        };

        let mut selected_b = Builder::new(400.0, 300.0, Some(&font));
        emit_tooltip_for_stack(
            &mut selected_b,
            &assets,
            &stack,
            Some([50.0, 50.0]),
            false,
            2,
            800,
            600,
            (400.0, 300.0),
            Some(1),
        );
        assert!(
            has_alpha(&selected_b.verts, BUNDLE_SLOT_HIGHLIGHT[3]),
            "a tracked selection must draw the highlight fill somewhere in the grid"
        );

        let mut unselected_b = Builder::new(400.0, 300.0, Some(&font));
        emit_tooltip_for_stack(
            &mut unselected_b,
            &assets,
            &stack,
            Some([50.0, 50.0]),
            false,
            2,
            800,
            600,
            (400.0, 300.0),
            None,
        );
        assert!(
            !has_alpha(&unselected_b.verts, BUNDLE_SLOT_HIGHLIGHT[3]),
            "control: no tracked selection must draw the highlight fill nowhere"
        );
    }

    /// [`emit_tooltip`] itself: the router-level wiring from a hovered slot's
    /// index plus a [`crate::container::bundle::BundleSelection`] down to the
    /// same highlight fill the function above exercises directly. Proves the
    /// window/slot filter in `emit_tooltip` passes the *matching* selection
    /// through rather than only that `emit_tooltip_for_stack` can draw one it
    /// is handed directly.
    #[test]
    fn emit_tooltip_forwards_a_selection_tracked_against_the_hovered_slot() {
        let Some(font) = VanillaFont::shared() else {
            return; // jar-less: nothing to measure against
        };
        let mut menu = lodestone_game::menu::Menu::generic(9);
        let stack = bundle_of(vec![
            ItemStack::new(id("minecraft:torch"), 1),
            ItemStack::new(id("minecraft:torch"), 1),
        ]);
        menu.set_slot_item(0, Some(stack));
        let assets = crate::hud::item_icon::IconAssets { items: None, models: None };
        let has_alpha = |verts: &[f32], alpha: f32| {
            verts.chunks_exact(6).any(|v| (v[5] - alpha).abs() < 1e-4)
        };

        let selection = crate::container::bundle::BundleSelection {
            window_id: 1,
            slot: 0,
            selected: 0,
        };
        let mut b = Builder::new(400.0, 300.0, Some(&font));
        emit_tooltip(
            &mut b,
            &assets,
            &menu,
            Some(0),
            Some([50.0, 50.0]),
            false,
            2,
            800,
            600,
            (400.0, 300.0),
            Some(selection),
        );
        assert!(
            has_alpha(&b.verts, BUNDLE_SLOT_HIGHLIGHT[3]),
            "a selection tracked against the hovered slot must reach the grid"
        );

        // Control: a selection tracked against a *different* slot must not
        // paint a highlight into this slot's grid.
        let mismatched = crate::container::bundle::BundleSelection {
            window_id: 1,
            slot: 5,
            selected: 0,
        };
        let mut b2 = Builder::new(400.0, 300.0, Some(&font));
        emit_tooltip(
            &mut b2,
            &assets,
            &menu,
            Some(0),
            Some([50.0, 50.0]),
            false,
            2,
            800,
            600,
            (400.0, 300.0),
            Some(mismatched),
        );
        assert!(
            !has_alpha(&b2.verts, BUNDLE_SLOT_HIGHLIGHT[3]),
            "control: a selection for a different slot must not highlight this one"
        );
    }

    // -------------------------------------------------------------------
    // Written books: name and tooltip, from the wire
    // -------------------------------------------------------------------

    /// A `minecraft:written_book`'s title, author and copy generation, taken
    /// **through the producer** rather than through a hand-built stack.
    ///
    /// This is the discipline CLAUDE.md's evidence section requires and the
    /// reason this gate is longer than the two assertions at the end of it: a
    /// tooltip fed a `lodestone_game::item::ItemStack` this test constructed
    /// would prove the tooltip and nothing about whether a real book ever
    /// reaches it. The chain here is production's, end to end:
    ///
    /// 1. `CONTAINER_SET_SLOT` bytes, transcribed from
    ///    `ClientboundContainerSetSlotPacket`'s and
    ///    `WrittenBookContent.STREAM_CODEC`'s wire order — **not** produced by
    ///    any encoder in this workspace, so a symmetric misunderstanding in
    ///    our own writer cannot make this pass.
    /// 2. The adapter `lodestone_registry::adapter_for_protocol(776)` returns
    ///    — the same call `net.rs`'s `run()` makes.
    /// 3. `lodestone_game::menus::Menus::apply`, the same fold `SessionMenus`
    ///    runs for every real `SET_SLOT`.
    /// 4. `styled_hover_name` and [`tooltip_lines`], read off the slot the
    ///    fold wrote.
    ///
    /// The `written_book_content` component was decodable before this and
    /// reached nowhere: `ItemStack::written_book_content` had **zero**
    /// production readers, so a signed book showed as "Written Book" with no
    /// author and no generation, which is what the report was.
    ///
    /// Fixture values are pairwise distinct on purpose — generation `2`
    /// (neither the `0` a fresh signature carries nor a value shared with any
    /// count in the frame), a title and author that are different words, and
    /// two differing pages — so a transposition of the two adjacent strings
    /// cannot survive.
    #[cfg(feature = "live")]
    #[test]
    fn a_written_book_off_the_wire_shows_its_title_author_and_generation() {
        use lodestone_model::{ClientEvent, ConnectionState, Directive};

        /// VarInt, 7 payload bits per byte with the MSB as continuation.
        fn varint(mut value: i32, out: &mut Vec<u8>) {
            loop {
                let byte = (value & 0x7F) as u8;
                value = ((value as u32) >> 7) as i32;
                if value == 0 {
                    out.push(byte);
                    return;
                }
                out.push(byte | 0x80);
            }
        }
        /// A protocol string: VarInt byte length, then UTF-8.
        fn string(value: &str, out: &mut Vec<u8>) {
            varint(i32::try_from(value.len()).expect("short fixture"), out);
            out.extend_from_slice(value.as_bytes());
        }
        /// A chat component in network NBT: the root tag id followed
        /// immediately by its payload, with no name — a plain-literal page is
        /// `TAG_String` (id 8), whose payload is a big-endian `u16` length
        /// then the bytes.
        fn nbt_string_component(value: &str, out: &mut Vec<u8>) {
            out.push(0x08);
            out.extend_from_slice(
                &u16::try_from(value.len()).expect("short fixture").to_be_bytes(),
            );
            out.extend_from_slice(value.as_bytes());
        }

        // The slot in `Menu`'s index space that is hotbar slot 0.
        const HOTBAR_0_MENU_SLOT: i16 = 36;
        // `minecraft:container_set_slot`'s protocol-776 clientbound id, from
        // Mojang's own `packets.json` (the same table
        // `lodestone_v770::packet_ids` is generated from). Written here
        // rather than imported because the shell must not name a protocol
        // family directly -- that is the version seam `just check-seam`
        // guards.
        const CONTAINER_SET_SLOT: i32 = 20;
        // `minecraft:written_book_content`'s data-component-type id,
        // resolved by searching the committed generated table through its own
        // public accessor rather than written as a literal, so a registry
        // reorder fails here instead of silently decoding as a neighbouring
        // component. The bound is generous -- the table is far shorter.
        let component_id = (0..1024)
            .find(|id| {
                lodestone_data::data_component_types::component_type_name(*id)
                    == Some("minecraft:written_book_content")
            })
            .expect("written_book_content is a real data component type");

        let mut payload = Vec::new();
        varint(0, &mut payload); // container id 0 -- the player inventory
        varint(0, &mut payload); // state id
        payload.extend_from_slice(&HOTBAR_0_MENU_SLOT.to_be_bytes());
        varint(1, &mut payload); // stack count
        varint(
            lodestone_data::items::item_id("minecraft:written_book")
                .expect("written_book is a real item"),
            &mut payload,
        );
        varint(1, &mut payload); // one added component
        varint(0, &mut payload); // no removed components
        varint(component_id, &mut payload);
        string("Wandering Notes", &mut payload); // title
        payload.push(0x00); // ...with no filtered alternate
        string("Steve", &mut payload); // author
        varint(2, &mut payload); // generation: a copy of a copy
        varint(2, &mut payload); // two pages
        nbt_string_component("First page", &mut payload);
        payload.push(0x00); // no filtered alternate
        nbt_string_component("Second page", &mut payload);
        payload.push(0x00);
        payload.push(0x01); // resolved

        let adapter = lodestone_registry::adapter_for_protocol(776)
            .expect("the `live` feature compiles a family in for protocol 776");
        let mut world = lodestone_world::World::new();
        let directives = adapter
            .handle_packet(
                &mut world,
                ConnectionState::Play,
                CONTAINER_SET_SLOT,
                &payload,
            )
            .expect("a byte-accurate SET_SLOT payload must decode");
        let Some(Directive::Emit(event @ ClientEvent::ContainerSlot { .. })) =
            directives.into_iter().next()
        else {
            panic!("expected a single ContainerSlot emit");
        };

        let mut menus = lodestone_game::menus::Menus::new();
        assert!(
            menus.apply(&event),
            "control: the fold must accept the event, or everything below reads \
             an untouched inventory"
        );
        let stack = menus
            .player_native(0)
            .expect("control: the book must land in hotbar slot 0")
            .clone();

        assert_eq!(
            lodestone_game::item::styled_hover_name(&stack, &|_| None),
            "Wandering Notes",
            "a signed book's display name is its own title -- vanilla's \
             `ItemStack.getCustomName()` falls back to \
             `written_book_content.title` -- not the item's \"Written Book\""
        );

        let lines = tooltip_lines(&stack, false);
        let lines: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            lines,
            vec!["Wandering Notes", "by Steve", "Copy of a copy"],
            "`WrittenBookContent.addToTooltip` adds `book.byAuthor` then \
             `book.generation.<n>`, under the title line"
        );
    }

    /// The negative half, and the control that the three lines above are the
    /// book's and not something every stack gets: an **unsigned**
    /// `minecraft:writable_book` is not a `TooltipProvider` in vanilla at all,
    /// so it keeps its plain item name and gains no author or generation line.
    #[test]
    fn an_unsigned_book_gains_no_author_or_generation_line() {
        let stack = ItemStack::new(
            "minecraft:writable_book".parse().expect("valid id"),
            1,
        );
        let lines = tooltip_lines(&stack, false);
        let lines: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(lines, vec!["Writable Book"]);
    }
}
