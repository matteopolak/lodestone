//! The merchant/trading screen's trade-list layout, prices and click
//! hit-test (that fix's UI half).
//!
//! ## What it is
//!
//! `MerchantScreen`'s left-hand trade list: seven scrollable rows of
//! cost/result icons that are **not menu slots** — vanilla calls them "fake
//! items" (`graphics.fakeItem`), rendered at fixed pixel offsets rather than
//! through [`super::layout::slot_layout`]. The three *real* slots (the two
//! payment slots and the take-only result) are ordinary
//! [`lodestone_game::menu::Menu`] slots and draw through the same path every
//! other container screen's slots do; this module only covers the list to
//! their left, the composed title, and the trade-row click.
//!
//! ## How it works
//!
//! [`row_layout`] and [`button_rect`] are vanilla's own pixel constants from
//! `MerchantScreen.java`, in **local widget pixels** (add the panel origin to
//! reach the canvas, exactly like [`super::layout::slot_layout`]'s
//! `SlotRect`s). [`button_hit_test`] resolves a physical cursor position to a
//! row index the same way [`super::layout::hit_test_with_book`] resolves one
//! to a slot, so a click and the drawn row can never disagree about which
//! pixel means what.
//!
//! [`cost_item_stack`] resolves a [`lodestone_model::event::MerchantOffer`]'s
//! raw `(item registry id, count)` cost pair into a displayable
//! [`ItemStack`] — the wire carries a registry id, not a name (an `ItemCost`
//! has no component data to build a real stack from), so this is the one
//! place in the shell that reaches into `lodestone-data`'s generated
//! protocol-776 item table rather than reading an already-resolved
//! [`lodestone_model::ItemStack`] off an event. [`adjusted_cost_a_count`]
//! ports `MerchantOffer.getModifiedCostCount` (`MerchantOffer.java`),
//! the demand/reputation pricing arithmetic that makes cost A's *displayed*
//! price differ from its base — the discount strikethrough vanilla draws when
//! they differ.
//!
//! ## How to change it
//!
//! The seven pixel constants below (`BUTTON_*`, `ITEM_*_X`, `OFFER_Y0`,
//! `ARROW_*`, `STRIKETHROUGH_*`) are transcribed from
//! `MerchantScreen.java` and its `init`/`extractContents`; re-derive
//! from `write`/the real screen, never from a summary, per this repo's own
//! transposition trap. Rows past [`OFFER_ROWS`] (offer index `>= 7`) are a
//! **named gap**: vanilla's scroller (`SCROLLER_SPRITE` et al.,
//! `MerchantScreen.java`) is not drawn or interactive here, so
//! a merchant with more than seven trades only shows and can only select the
//! first seven. The experience-bar trio
//! (`EXPERIENCE_BAR_*`, `extractProgressBar`) is a second named gap: it needs
//! `VillagerData.getMinXpPerLevel`/`getMaxXpPerLevel`, thresholds this tree
//! does not carry yet.
//!
//! ## Configuration
//!
//! None — pure pixel arithmetic and a linear scan over `lodestone-data`'s
//! item table.
//!
//! ## Dependencies
//!
//! [`lodestone_data::items`] (protocol-776 item id→name), [`lodestone_game`]
//! (`ItemStack`, `TradeOffers`), [`lodestone_model::event::MerchantOffer`].

use lodestone_game::item::{DEFAULT_MAX_STACK_SIZE, ItemStack};
use lodestone_model::event::MerchantOffer;

use super::layout::Rect;

/// `MerchantScreen`'s `imageWidth`/`imageHeight` (`MerchantScreen.java`).
pub const PANEL_W: f32 = 276.0;
/// See [`PANEL_W`].
pub const PANEL_H: f32 = 166.0;

/// `MerchantScreen.NUMBER_OF_OFFER_BUTTONS` — the number of trade rows shown
/// at once. See the module doc's "how to change it" for what showing more
/// than this would need.
pub const OFFER_ROWS: usize = 7;

/// `TRADE_BUTTON_X`/`TRADE_BUTTON_WIDTH`/`TRADE_BUTTON_HEIGHT`
/// (`MerchantScreen.java`) and `init`'s own `buttonY = yo + 16 + 2`
/// (`MerchantScreen.java`), in local widget pixels (panel top-left is
/// `(0, 0)`).
const BUTTON_X: f32 = 5.0;
const BUTTON_Y0: f32 = 18.0;
const BUTTON_W: f32 = 88.0;
const BUTTON_H: f32 = 20.0;
/// Vertical step between rows — both the buttons' `buttonY += 20` and the
/// items' `offerY += 20` (`MerchantScreen.java`) share this constant.
const ROW_STEP: f32 = 20.0;

/// `extractContents`'s `offerY = yo + 16 + 1` then `decorHeight = offerY + 2`
/// (`MerchantScreen.java`) — the fake items' shared row-0 y. Distinct
/// from [`BUTTON_Y0`] by one pixel, matching vanilla's own two separate
/// tracks.
const OFFER_Y0: f32 = 19.0;
/// `SELL_ITEM_1_X` plus `extractContents`'s own `+ 5` (`MerchantScreen.java`).
const ITEM_A_X: f32 = 10.0;
/// `SELL_ITEM_2_X` (`MerchantScreen.java`), relative to the panel (the `xo
/// + 5 +` in `extractContents` folds to the same `35` offset from
/// [`ITEM_A_X`]'s own `xo + 5 +`).
const ITEM_B_X: f32 = 40.0;
/// `BUY_ITEM_X` (`MerchantScreen.java`), same fold as [`ITEM_B_X`].
const ITEM_RESULT_X: f32 = 73.0;
/// `extractButtonArrows`'s `xo + 5 + 35 + 20` (`MerchantScreen.java`).
const ARROW_X: f32 = 60.0;
/// `extractButtonArrows`'s `decorHeight + 3` — offset from a row's own
/// [`OFFER_Y0`]-based y, not from the panel origin.
const ARROW_Y_OFFSET: f32 = 3.0;
/// The trade-arrow sprite's own size — see [`super::geometry`]'s one caller.
pub const ARROW_W: f32 = 10.0;
/// See [`ARROW_W`].
pub const ARROW_H: f32 = 9.0;
/// `extractAndDecorateCostA`'s `sellItem1X + 7`/`decorHeight + 12`
/// (`MerchantScreen.java`), offset from [`ITEM_A_X`]/a row's y.
const STRIKETHROUGH_X_OFFSET: f32 = 7.0;
const STRIKETHROUGH_Y_OFFSET: f32 = 12.0;
/// The discount-strikethrough sprite's own size — see [`super::geometry`]'s
/// one caller.
pub const STRIKETHROUGH_W: f32 = 9.0;
/// See [`STRIKETHROUGH_W`].
pub const STRIKETHROUGH_H: f32 = 2.0;

/// `extractBackground`'s out-of-stock overlay,
/// `this.leftPos + 83 + 99, this.topPos + 35, 28, 21`
/// (`MerchantScreen.java`) — a single overlay for the **selected** row,
/// not one per row, so it is a fixed panel-relative position rather than a
/// [`row_layout`] field.
pub const OUT_OF_STOCK_X: f32 = 182.0;
pub const OUT_OF_STOCK_Y: f32 = 35.0;
pub const OUT_OF_STOCK_W: f32 = 28.0;
pub const OUT_OF_STOCK_H: f32 = 21.0;

/// One trade row's item/arrow/discount positions, in local widget pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowLayout {
    /// First cost's icon position.
    pub cost_a: [f32; 2],
    /// Second cost's icon position (drawn only when the offer has one).
    pub cost_b: [f32; 2],
    /// Result icon position.
    pub result: [f32; 2],
    /// The trade arrow (normal or out-of-stock) between cost B and the result.
    pub arrow: [f32; 2],
    /// The discount strikethrough sprite, drawn only when cost A's adjusted
    /// price differs from its base.
    pub strikethrough: [f32; 2],
}

/// Row `i`'s layout (`i` is **not** clamped to [`OFFER_ROWS`] — callers
/// already iterate `0..OFFER_ROWS.min(offers.len())`, per [`visible_rows`]).
#[must_use]
pub fn row_layout(i: usize) -> RowLayout {
    let y = OFFER_Y0 + ROW_STEP * i as f32;
    RowLayout {
        cost_a: [ITEM_A_X, y],
        cost_b: [ITEM_B_X, y],
        result: [ITEM_RESULT_X, y],
        arrow: [ARROW_X, y + ARROW_Y_OFFSET],
        strikethrough: [ITEM_A_X + STRIKETHROUGH_X_OFFSET, y + STRIKETHROUGH_Y_OFFSET],
    }
}

/// Row `i`'s clickable button rect, in local widget pixels — see
/// [`button_hit_test`].
#[must_use]
pub fn button_rect(i: usize) -> Rect {
    Rect {
        x: BUTTON_X,
        y: BUTTON_Y0 + ROW_STEP * i as f32,
        w: BUTTON_W,
        h: BUTTON_H,
    }
}

/// How many of `offers` are drawn/clickable this frame — `offers.len()`
/// clamped to [`OFFER_ROWS`]. See the module doc's scrolling gap.
#[must_use]
pub fn visible_row_count(offer_count: usize) -> usize {
    offer_count.min(OFFER_ROWS)
}

/// Resolves a **local widget-pixel** point to a trade-row index, or `None` if
/// it hits no row's button. `offer_count` is `TradeOffers::offers().len()`;
/// only rows within [`visible_row_count`] are tested.
#[must_use]
pub fn hit_test_local(offer_count: usize, x: f32, y: f32) -> Option<usize> {
    for i in 0..visible_row_count(offer_count) {
        let rect = button_rect(i);
        if x >= rect.x && x < rect.x + rect.w && y >= rect.y && y < rect.y + rect.h {
            return Some(i);
        }
    }
    None
}

/// [`hit_test_local`], resolving from a **physical viewport** cursor position
/// the same way [`super::layout::hit_test_with_book`] resolves a slot click —
/// `menu` must have [`lodestone_game::menu::SpecialLayout::Merchant`] or this
/// returns `None` unconditionally (there is no trade list to click on any
/// other screen).
#[must_use]
pub fn button_hit_test(
    menu: &lodestone_game::menu::Menu,
    offer_count: usize,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> Option<usize> {
    if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Merchant) {
        return None;
    }
    let layout = super::layout::slot_layout(menu);
    let (px, py) = super::layout::panel_origin_with_scale(&layout, gui_scale, width, height);
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    hit_test_local(offer_count, x / scale - px, y / scale - py)
}

/// `MerchantOffer.getModifiedCostCount` (`MerchantOffer.java`) — cost
/// A's demand/reputation-adjusted price:
///
/// ```java
/// int demandDiff = Math.max(0, Mth.floor(basePrice * this.demand * this.priceMultiplier));
/// return Mth.clamp(basePrice + demandDiff + this.specialPriceDiff, 1, cost.itemStack().getMaxStackSize());
/// ```
///
/// The clamp's upper bound uses [`DEFAULT_MAX_STACK_SIZE`] rather than the
/// resolved item's real max stack size: `cost_a`'s wire form has no component
/// data (see the module doc), so a handful of items whose real cap is below
/// 64 can show a one-off-wrong ceiling here. Server-side pricing is
/// unaffected — this is a display-only approximation.
#[must_use]
pub fn adjusted_cost_a_count(offer: &MerchantOffer) -> i32 {
    let base = offer.cost_a.1;
    #[allow(clippy::cast_precision_loss)]
    let demand_diff =
        ((base as f32) * (offer.demand as f32) * offer.price_multiplier).floor().max(0.0);
    #[allow(clippy::cast_possible_truncation)]
    let demand_diff = demand_diff as i32;
    (base + demand_diff + offer.special_price_diff).clamp(1, DEFAULT_MAX_STACK_SIZE)
}

/// Resolves a raw `(item registry id, count)` cost pair (protocol 776, the
/// only family with a merchant screen — `lodestone_data::items::item_name`'s
/// own doc names it "for protocol 776") into a displayable [`ItemStack`].
///
/// `None` for an id outside the generated table (a malformed or
/// future-version id), which the caller should treat as "draw nothing" rather
/// than a default item — inventing an item here would be a silently wrong
/// icon, not a missing one.
#[must_use]
pub fn cost_item_stack(id: i32, count: i32) -> Option<ItemStack> {
    let name = lodestone_data::items::item_name(id)?;
    let identifier: lodestone_model::Identifier = name.parse().ok()?;
    Some(ItemStack::new(identifier, count.max(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(cost_a: (i32, i32), demand: i32, price_multiplier: f32, special_price_diff: i32) -> MerchantOffer {
        MerchantOffer {
            cost_a,
            cost_b: None,
            result: None,
            out_of_stock: false,
            uses: 0,
            max_uses: 12,
            xp: 1,
            special_price_diff,
            price_multiplier,
            demand,
        }
    }

    #[test]
    fn row_layout_matches_vanillas_transcribed_constants() {
        // Row 0: MerchantScreen.java's own arithmetic with xo/yo = 0.
        let row0 = row_layout(0);
        assert_eq!(row0.cost_a, [10.0, 19.0]);
        assert_eq!(row0.cost_b, [40.0, 19.0]);
        assert_eq!(row0.result, [73.0, 19.0]);
        assert_eq!(row0.arrow, [60.0, 22.0]);
        assert_eq!(row0.strikethrough, [17.0, 31.0]);
        // Row 1 steps every field by exactly ROW_STEP (20px) in y, x unchanged.
        let row1 = row_layout(1);
        assert_eq!(row1.cost_a, [10.0, 39.0]);
        assert_eq!(row1.arrow, [60.0, 42.0]);
    }

    #[test]
    fn button_rect_steps_by_row_and_starts_one_pixel_off_the_items() {
        // Vanilla's own two tracks (buttonY starts at 18, offerY's decorHeight
        // at 19) must not be conflated into one.
        assert_eq!(button_rect(0), Rect { x: 5.0, y: 18.0, w: 88.0, h: 20.0 });
        assert_eq!(button_rect(3), Rect { x: 5.0, y: 78.0, w: 88.0, h: 20.0 });
    }

    #[test]
    fn hit_test_local_finds_the_right_row_and_nothing_between_rows() {
        assert_eq!(hit_test_local(7, 40.0, 25.0), Some(0));
        assert_eq!(hit_test_local(7, 40.0, 45.0), Some(1));
        // Between row 0's bottom (38) and row 1's top (38) there is no gap —
        // rows are contiguous — but well outside the button's x range must miss.
        assert_eq!(hit_test_local(7, 200.0, 25.0), None);
        // Above every button.
        assert_eq!(hit_test_local(7, 40.0, 5.0), None);
    }

    #[test]
    fn hit_test_local_never_finds_a_row_past_offer_count_or_seven() {
        // Only 2 offers: row 2's button rect exists geometrically but must not
        // register as a hit.
        assert_eq!(hit_test_local(2, 40.0, 65.0), None);
        // 20 offers: row 7 (the 8th) is past OFFER_ROWS and must not register
        // either — the named scrolling gap.
        assert_eq!(hit_test_local(20, 40.0, 18.0 + 20.0 * 7.0 + 1.0), None);
    }

    #[test]
    fn adjusted_cost_a_count_applies_demand_then_special_price_then_clamps() {
        // Base 10, demand 4, multiplier 0.05 -> demandDiff = floor(10*4*0.05) = 2.
        // +special_price_diff 1 -> 13.
        let o = offer((1, 10), 4, 0.05, 1);
        assert_eq!(adjusted_cost_a_count(&o), 13);
        // A large negative special_price_diff clamps to 1, not below.
        let discounted = offer((1, 10), 0, 0.0, -50);
        assert_eq!(adjusted_cost_a_count(&discounted), 1);
        // Zero demand/multiplier/special_price_diff: unchanged from base — the
        // "no discount, draw one icon" case `geometry.rs` keys off.
        let plain = offer((1, 5), 0, 0.0, 0);
        assert_eq!(adjusted_cost_a_count(&plain), 5);
    }

    #[test]
    fn cost_item_stack_resolves_a_real_id_and_rejects_an_out_of_range_one() {
        // id 1 is minecraft:stone in the 26.2 registry order (id 0 is air).
        let stack = cost_item_stack(1, 3).expect("id 1 resolves");
        assert_eq!(stack.item().to_string(), "minecraft:stone");
        assert_eq!(stack.count(), 3);
        assert!(cost_item_stack(i32::MAX, 1).is_none(), "an absurd id must resolve to nothing, not a wrong item");
    }
}
