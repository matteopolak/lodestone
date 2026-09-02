//! The enchanting table's three enchant-offer buttons (issue #613's
//! `ContainerButtonClick` remainder).
//!
//! ## What it is
//!
//! `ClientAction::ContainerButtonClick` was encoded by every protocol family
//! with zero shell callers — the same outbound-island shape
//! `ClientAction::SetFlying`/`SetBeaconEffects` were caught in before their
//! own fixes. This module is the producer for the one screen 26.2 actually
//! uses it for that this tree already renders: the enchanting table's three
//! offer rows (vanilla's own enchantment-screen click handling and its own
//! enchantment-menu button-click handling). Vanilla's other two
//! `ContainerButtonClick` screens, the stonecutter and the loom, pick a
//! *recipe*/*pattern* from a server-populated list this tree has no registry
//! sync for yet (vanilla's own stonecutter/loom menus' selectable-recipes mechanism)
//! — out of scope here, see `docs/container-cost-screens.md`.
//!
//! ## How it works
//!
//! [`offer_rect`] is vanilla's own enchantment-screen click handling's own click rect
//! (`xo + 60, yo + 14 + 19*i, 108, 19`) — the exact same local-widget-pixel
//! geometry `container::geometry::draw_enchanting_costs` already draws the
//! cost numbers at, so the clickable area and the drawn button always agree.
//! [`offer_clickable`] is vanilla's own enchantment-menu button-click
//! handling's own gate: it
//! runs **client-side too** (the client's own `EnchantmentMenu` mirror calls
//! it before sending anything), but its `access.execute` is a no-op there —
//! so on the client it only ever answers "is this click worth sending",
//! never mutates anything, the same "predict, then send" split every other
//! click surface in this crate already follows. [`button_hit_test`] is
//! [`super::beacon::button_hit_test`]'s shape: hit-test plus the panel
//! origin/scale resolution every click surface in this crate goes through.
//!
//! ## How to change it
//!
//! `container_data` properties `0..3` are `EnchantmentMenu.costs[0..3]`
//! (`container::geometry::draw_enchanting_costs` already reads the same
//! three); the lapis slot is menu slot index 1
//! (`Menu::enchanting_table`/`docs/container-cost-screens.md`).
//!
//! ## Dependencies
//!
//! [`super::layout`] (panel origin/scale — the same seam every other click
//! surface in this crate resolves a cursor through).

use lodestone_game::menu::{Menu, SpecialLayout};

use super::layout::Rect;

/// One offer row's local-widget-pixel rect — `EnchantmentScreen.mouseClicked`'s
/// own `xo + 60, yo + 14 + 19*i, 108, 19` (`i` is `row`, `0..3`).
#[must_use]
#[allow(clippy::cast_precision_loss)] // row is always 0..3
pub fn offer_rect(row: i32) -> Rect {
    Rect {
        x: 60.0,
        y: 14.0 + 19.0 * row as f32,
        w: 108.0,
        h: 19.0,
    }
}

/// `EnchantmentMenu.clickMenuButton`'s client-visible gate: a lapis count of
/// at least `row + 1`, a non-zero offer cost, and an experience level meeting
/// both the row cost and the flat `row + 1` requirement — every check
/// skipped outright when `has_infinite_materials` (creative) is set, exactly
/// as vanilla's own `!player.hasInfiniteMaterials()` guards each clause.
#[must_use]
pub fn offer_clickable(
    cost: i32,
    row: i32,
    lapis_count: i32,
    xp_level: i32,
    has_infinite_materials: bool,
) -> bool {
    if cost <= 0 {
        return false;
    }
    let enchantment_cost = row + 1;
    if has_infinite_materials {
        return true;
    }
    if lapis_count < enchantment_cost {
        return false;
    }
    xp_level >= enchantment_cost && xp_level >= cost
}

fn hit(x: f32, y: f32, r: Rect) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// Resolves a **local widget-pixel** point to the offer row it hits, if any
/// and currently clickable — a click landing on a disabled row falls through
/// as if nothing were there, matching [`super::beacon::hit_test_local`]'s
/// identical treatment of an unlocked-tier miss.
#[must_use]
pub fn hit_test_local(
    costs: [i32; 3],
    lapis_count: i32,
    xp_level: i32,
    has_infinite_materials: bool,
    x: f32,
    y: f32,
) -> Option<i32> {
    for row in 0..3i32 {
        if hit(x, y, offer_rect(row))
            && offer_clickable(costs[row as usize], row, lapis_count, xp_level, has_infinite_materials)
        {
            return Some(row);
        }
    }
    None
}

/// [`hit_test_local`] plus the panel-origin/scale resolution every other
/// click surface in this crate does — the same shape as
/// [`super::beacon::button_hit_test`]. `None` off any non-enchanting screen.
#[must_use]
#[allow(clippy::too_many_arguments)] // mirrors button_hit_test's own shape
pub fn button_hit_test(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    costs: [i32; 3],
    lapis_count: i32,
    xp_level: i32,
    has_infinite_materials: bool,
) -> Option<i32> {
    if menu.special_layout() != Some(SpecialLayout::Enchanting) {
        return None;
    }
    let layout = super::layout::slot_layout(menu);
    let (px, py) = super::layout::panel_origin_with_scale(&layout, gui_scale, width, height);
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    hit_test_local(
        costs,
        lapis_count,
        xp_level,
        has_infinite_materials,
        x / scale - px,
        y / scale - py,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_rect_matches_vanillas_transcribed_arithmetic() {
        assert_eq!(offer_rect(0), Rect { x: 60.0, y: 14.0, w: 108.0, h: 19.0 });
        assert_eq!(offer_rect(1), Rect { x: 60.0, y: 33.0, w: 108.0, h: 19.0 });
        assert_eq!(offer_rect(2), Rect { x: 60.0, y: 52.0, w: 108.0, h: 19.0 });
    }

    #[test]
    fn a_zero_cost_row_is_never_clickable() {
        assert!(!offer_clickable(0, 0, 64, 30, false));
        assert!(!offer_clickable(0, 0, 64, 30, true), "not even with infinite materials");
    }

    #[test]
    fn insufficient_lapis_blocks_the_click() {
        // Row 2 needs 3 lapis; a real cost (e.g. 9) and plenty of xp.
        assert!(!offer_clickable(9, 2, 2, 30, false));
        assert!(offer_clickable(9, 2, 3, 30, false));
    }

    #[test]
    fn insufficient_experience_level_blocks_the_click() {
        // Row 0 needs level >= 1 and >= cost.
        assert!(!offer_clickable(5, 0, 10, 0, false), "level 0 cannot afford any offer");
        assert!(!offer_clickable(5, 0, 10, 3, false), "level 3 < cost 5");
        assert!(offer_clickable(5, 0, 10, 5, false));
    }

    #[test]
    fn infinite_materials_bypasses_lapis_and_level_but_not_a_zero_cost() {
        assert!(offer_clickable(9, 2, 0, 0, true));
        assert!(!offer_clickable(0, 2, 99, 99, true));
    }

    #[test]
    fn hit_test_finds_the_row_a_point_falls_in_when_clickable() {
        let costs = [3, 0, 9];
        // Row 0: cost 3, needs lapis >= 1 and level >= 3.
        let r0 = offer_rect(0);
        assert_eq!(
            hit_test_local(costs, 1, 3, false, r0.x + 1.0, r0.y + 1.0),
            Some(0)
        );
        // Row 1 has cost 0 -- disabled regardless of resources.
        let r1 = offer_rect(1);
        assert_eq!(hit_test_local(costs, 99, 99, false, r1.x + 1.0, r1.y + 1.0), None);
        // Row 2: cost 9, needs lapis >= 3 and level >= 9 -- affordable here.
        let r2 = offer_rect(2);
        assert_eq!(
            hit_test_local(costs, 3, 9, false, r2.x + 1.0, r2.y + 1.0),
            Some(2)
        );
        // Outside every rect: no hit.
        assert_eq!(hit_test_local(costs, 3, 9, false, -5.0, -5.0), None);
    }

    #[test]
    fn hit_test_ignores_an_affordable_row_the_click_missed() {
        let costs = [3, 5, 9];
        // Past row 0's right edge, on the same y -- the row's horizontal
        // bound must be respected, not just its vertical one.
        let r0 = offer_rect(0);
        assert_eq!(
            hit_test_local(costs, 99, 99, false, r0.x + r0.w + 1.0, r0.y + 1.0),
            None
        );
    }
}
