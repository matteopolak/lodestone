//! The stonecutter's recipe-selection scroll list (`ClientAction::
//! ContainerButtonClick`'s remainder for this screen — see
//! [`super::enchant`]'s module doc for the enchanting table's own producer,
//! the precedent this module follows).
//!
//! ## What it is
//!
//! `StonecutterScreen`/`StonecutterMenu` (`26.2`): a 4×3 grid of up to twelve
//! visible recipe buttons, scrollable when the server reports more than
//! twelve stonecutting recipes for the input item. [`server_results_for_menu`]
//! is the one source used to draw, scroll and pre-validate clicks, so a
//! server's datapack cannot disagree with a bundled client recipe corpus.
//!
//! ## How it works
//!
//! [`server_results_for_menu`] resolves the active input item to its numeric
//! registry id and reads the ordered result rows retained by
//! [`lodestone_game::recipe_sync::RecipeBookSync`]. Each row remains present
//! even when this build cannot resolve its icon, preserving the server's
//! button indices for later rows.
//!
//! [`grid_rect`] is `StonecutterScreen`'s own real layout constants
//! (`RECIPES_X = 52`, `RECIPES_Y = 14`, a 16×18 cell, 4 columns) —
//! [`hit_test_local`] mirrors `StonecutterScreen.mouseClicked`'s exact
//! arithmetic, `start_index`-relative, and [`button_hit_test`] adds the panel
//! origin/scale resolution every click surface in this crate goes through.
//!
//! ## How to change it
//!
//! Keep redraw, wheel and click consumers on [`server_results_for_menu`]; a
//! second local derivation can silently reorder button ids. Scrolling
//! ([`start_index_for_scroll`]/[`scroll_offset_after_wheel`]) persists on
//! `WindowApp::stonecutter_scroll`, and that same start index is attached to
//! `ContainerFrame` for drawing and used for hit-testing. The scrollbar thumb
//! drag is still not wired; the wheel reaches every result past twelve.
//!
//! ## Dependencies
//!
//! [`lodestone_game::recipe_sync`] for the server-declared result rows,
//! [`super::layout`] for the panel origin/scale seam every other click
//! surface in this crate resolves a cursor through.

use lodestone_game::item::ItemStack;
use lodestone_game::menu::{Menu, SpecialLayout};
use lodestone_game::recipe_sync::RecipeBookSync;
use lodestone_data::item::Item;
use lodestone_model::Identifier;

use super::layout::Rect;

/// `StonecutterMenu`'s own input slot index (`Menu::stonecutter`'s doc).
pub const INPUT_SLOT: usize = 0;

/// `StonecutterScreen.RECIPES_X`/`RECIPES_Y`/cell size/column count.
const GRID_X: f32 = 52.0;
const GRID_Y: f32 = 14.0;
const CELL_W: f32 = 16.0;
const CELL_H: f32 = 18.0;
const COLUMNS: i32 = 4;
/// `StonecutterScreen`'s three visible rows (twelve visible buttons at once).
const VISIBLE_ROWS: i32 = 3;
const VISIBLE_COUNT: i32 = COLUMNS * VISIBLE_ROWS;

/// Resolves one server-sent stonecutter recipe's raw result-item candidates
/// (one entry of
/// [`lodestone_game::recipe_sync::RecipeBookSync::stonecutter_results_for`]'s
/// output) to a single drawable stack — the first id this build's item table
/// can name, the same "pick one, do not guess a name" contract
/// [`super::merchant::cost_item_stack`] and `app/recipe_panel.rs`'s
/// `ghost_result_stack` both already keep. `None` when no candidate resolves,
/// which draws nothing rather than a guessed icon.
#[must_use]
pub fn server_result_stack(result_items: &[i32]) -> Option<ItemStack> {
    result_items.iter().find_map(|&id| {
        let item = u16::try_from(id).ok().and_then(Item::from_registry_id)?;
        let identifier: Identifier = item.name().parse().ok()?;
        Some(ItemStack::new(identifier, 1))
    })
}

/// The server's authoritative result rows for the active stonecutter input.
///
/// The returned vector has one element per server row. An unresolvable result
/// is `None`, not removed: its blank cell must keep occupying the original
/// button id so every later visible icon still sends the index the server
/// assigned it. Empty for another screen, an empty/unknown input, or no rows.
#[must_use]
pub fn server_results_for_menu(menu: &Menu, sync: &RecipeBookSync) -> Vec<Option<ItemStack>> {
    if menu.special_layout() != Some(SpecialLayout::Stonecutter) {
        return Vec::new();
    }
    let Some(input) = menu.slot_item(INPUT_SLOT) else {
        return Vec::new();
    };
    let Some(input_item_id) = Item::from_name(&input.item().to_string())
        .map(|item| i32::from(item.registry_id()))
    else {
        return Vec::new();
    };
    sync.stonecutter_results_for(input_item_id)
        .map(server_result_stack)
        .collect()
}

/// The at-most-twelve drawable rows in the current page, paired with their
/// original server button ids. Pagination happens before unresolved icons are
/// filtered so neither gaps nor scrolling can renumber a later result.
pub(super) fn visible_server_results(
    results: &[Option<ItemStack>],
    start_index: i32,
) -> impl Iterator<Item = (i32, &ItemStack)> {
    let start = usize::try_from(start_index).unwrap_or(0);
    results
        .iter()
        .enumerate()
        .skip(start)
        .take(VISIBLE_COUNT as usize)
        .filter_map(|(index, stack)| Some((i32::try_from(index).ok()?, stack.as_ref()?)))
}

/// One recipe button's local-widget-pixel rect, `index`-relative to
/// `start_index` — `StonecutterScreen.extractButtons`'s `posX`/`posY`
/// (`x + posIndex % 4 * 16`, `y + row * 18 + 2`).
#[must_use]
#[allow(clippy::cast_precision_loss)] // index/start_index are always small
pub fn grid_rect(index: i32, start_index: i32) -> Option<Rect> {
    let pos_index = index - start_index;
    if !(0..VISIBLE_COUNT).contains(&pos_index) {
        return None;
    }
    let col = pos_index % COLUMNS;
    let row = pos_index / COLUMNS;
    Some(Rect {
        x: GRID_X + col as f32 * CELL_W,
        y: GRID_Y + row as f32 * CELL_H + 2.0,
        w: CELL_W,
        h: CELL_H,
    })
}

fn hit(x: f32, y: f32, r: Rect) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// Resolves a **local widget-pixel** point to the recipe index it hits, if
/// any — `StonecutterScreen.mouseClicked`'s own loop, `start_index`-relative,
/// bounded by `recipe_count` (vanilla's `getNumberOfVisibleRecipes()`, not
/// `endIndex` alone: a partially-filled last row must not accept a click past
/// the real recipe count even though its cell rect exists).
#[must_use]
pub fn hit_test_local(recipe_count: usize, start_index: i32, x: f32, y: f32) -> Option<i32> {
    let end_index = start_index + VISIBLE_COUNT;
    for index in start_index..end_index {
        if index >= 0
            && (index as usize) < recipe_count
            && let Some(r) = grid_rect(index, start_index)
            && hit(x, y, r)
        {
            return Some(index);
        }
    }
    None
}

/// `StonecutterScreen.getOffscreenRows`: `ceil(recipe_count / 4) - 3`, floored
/// at `0` (vanilla lets this go negative internally but only ever multiplies
/// it by a `0.0..=1.0` `scrollOffs`, so clamping here is behaviourally
/// identical and avoids a negative `start_index`).
#[must_use]
fn offscreen_rows(recipe_count: usize) -> i32 {
    let rows = (recipe_count as i32 + COLUMNS - 1) / COLUMNS - VISIBLE_ROWS;
    rows.max(0)
}

/// `StonecutterScreen.mouseDragged`/`mouseScrolled`'s shared tail:
/// `startIndex = (scrollOffs * offscreenRows + 0.5) * 4`, `scroll_offset`
/// clamped to `0.0..=1.0` first exactly as vanilla clamps it before either
/// call site uses it.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // matches vanilla's own (int) cast
pub fn start_index_for_scroll(scroll_offset: f32, recipe_count: usize) -> i32 {
    let clamped = scroll_offset.clamp(0.0, 1.0);
    let rows = offscreen_rows(recipe_count) as f32;
    ((clamped * rows + 0.5) as i32) * COLUMNS
}

/// `StonecutterScreen.mouseScrolled`'s own step: `scrollOffs = clamp(scrollOffs
/// - scrollY / offscreenRows, 0, 1)`, gated on `isScrollBarActive()` the same
/// way vanilla's own `if (this.isScrollBarActive())` guards the whole method
/// body — a no-op (returns `current` unchanged, pinned at `0.0`) when there is
/// nothing offscreen, never dividing by zero. Wired to the mouse wheel by
/// `WindowApp::scroll_stonecutter` (`app/container_input.rs`), the missing
/// half this module's own doc used to name.
#[must_use]
pub fn scroll_offset_after_wheel(current: f32, notches: f32, recipe_count: usize) -> f32 {
    let rows = offscreen_rows(recipe_count);
    if rows <= 0 {
        return 0.0;
    }
    (current - notches / rows as f32).clamp(0.0, 1.0)
}

/// [`hit_test_local`] plus the panel-origin/scale resolution every other
/// click surface in this crate does — the same shape as
/// [`super::enchant::button_hit_test`]. `None` off any non-stonecutter
/// screen.
#[must_use]
pub fn button_hit_test(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    recipe_count: usize,
    start_index: i32,
) -> Option<i32> {
    if menu.special_layout() != Some(SpecialLayout::Stonecutter) {
        return None;
    }
    let layout = super::layout::slot_layout(menu);
    let (px, py) = super::layout::panel_origin_with_scale(&layout, gui_scale, width, height);
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    hit_test_local(recipe_count, start_index, x / scale - px, y / scale - py)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Identifier {
        s.parse().unwrap()
    }

    /// A resolvable id must produce a real stack — the positive half, so the
    /// negative control below is not vacuously true of a function that
    /// always returns `None`.
    #[test]
    fn server_result_stack_resolves_the_first_nameable_candidate() {
        let stone_slab = i32::from(Item::StoneSlab.registry_id());
        let stack =
            server_result_stack(&[stone_slab]).expect("a real id must resolve");
        assert_eq!(stack.item().to_string(), "minecraft:stone_slab");
        assert_eq!(stack.count(), 1);
    }

    /// The executed negative control: an id outside the generated table
    /// (i.e. no real item registered at it) resolves to nothing rather than
    /// a guessed icon.
    #[test]
    fn server_result_stack_is_none_for_an_id_outside_the_table() {
        assert!(server_result_stack(&[i32::MAX]).is_none());
    }

    /// A tag-shaped display can offer several candidate ids for the same
    /// recipe; the first nameable one wins, matching
    /// `app/recipe_panel.rs`'s `ghost_result_stack`.
    #[test]
    fn server_result_stack_skips_an_unresolvable_leading_candidate() {
        let stone_slab = i32::from(Item::StoneSlab.registry_id());
        let stack = server_result_stack(&[i32::MAX, stone_slab]).expect("the second id must resolve");
        assert_eq!(stack.item().to_string(), "minecraft:stone_slab");
    }

    #[test]
    fn server_results_for_menu_preserves_server_indices_across_unresolvable_entries() {
        let stone = i32::from(Item::Stone.registry_id());
        let stone_slab = i32::from(Item::StoneSlab.registry_id());
        let stone_stairs = i32::from(Item::StoneStairs.registry_id());
        let mut menu = Menu::stonecutter();
        menu.set_slot_item(
            INPUT_SLOT,
            Some(ItemStack::new(id("minecraft:stone"), 1)),
        );
        let mut sync = lodestone_game::recipe_sync::RecipeBookSync::new();
        sync.apply(&lodestone_model::event::ClientEvent::RecipePropertySetsUpdated {
            item_sets: Vec::new(),
            stonecutter_results: vec![
                (vec![stone], vec![stone_slab]),
                (vec![stone], vec![i32::MAX]),
                (vec![stone], vec![stone_stairs]),
            ],
        });

        let results = server_results_for_menu(&menu, &sync);
        assert_eq!(
            results.len(),
            3,
            "wire indices must not collapse when one icon cannot resolve"
        );
        assert_eq!(results[0].as_ref().unwrap().item().path(), "stone_slab");
        assert!(
            results[1].is_none(),
            "an unknown item draws an empty cell at its server index"
        );
        assert_eq!(results[2].as_ref().unwrap().item().path(), "stone_stairs");

        let visible: Vec<_> = visible_server_results(&results, 1)
            .map(|(index, stack)| (index, stack.item().path().to_owned()))
            .collect();
        assert_eq!(visible, vec![(2, "stone_stairs".to_owned())]);

        let twenty = vec![results[0].clone(); 20];
        assert_eq!(
            visible_server_results(&twenty, 4)
                .map(|(index, _)| index)
                .collect::<Vec<_>>(),
            (4..16).collect::<Vec<_>>(),
            "drawing skips to the frame start, takes twelve, and keeps absolute indices"
        );
    }

    #[test]
    fn server_results_for_menu_rejects_non_stonecutter_and_empty_input() {
        let sync = lodestone_game::recipe_sync::RecipeBookSync::new();
        assert!(server_results_for_menu(&Menu::stonecutter(), &sync).is_empty());
        assert!(server_results_for_menu(&Menu::generic(9), &sync).is_empty());
    }

    #[test]
    fn grid_rect_matches_the_transcribed_stonecutter_screen_arithmetic() {
        assert_eq!(grid_rect(0, 0), Some(Rect { x: 52.0, y: 16.0, w: 16.0, h: 18.0 }));
        assert_eq!(grid_rect(3, 0), Some(Rect { x: 100.0, y: 16.0, w: 16.0, h: 18.0 }));
        assert_eq!(grid_rect(4, 0), Some(Rect { x: 52.0, y: 34.0, w: 16.0, h: 18.0 }));
        // Scrolled: index 12 with start_index 4 is posIndex 8, row 2 col 0.
        assert_eq!(grid_rect(12, 4), Some(Rect { x: 52.0, y: 52.0, w: 16.0, h: 18.0 }));
        // Out of the visible 4x3 window relative to start_index: no rect.
        assert_eq!(grid_rect(12, 0), None);
    }

    #[test]
    fn hit_test_finds_the_index_a_point_falls_in() {
        let r = grid_rect(5, 0).unwrap();
        assert_eq!(hit_test_local(12, 0, r.x + 1.0, r.y + 1.0), Some(5));
        assert_eq!(hit_test_local(12, 0, -5.0, -5.0), None);
    }

    #[test]
    fn hit_test_refuses_a_cell_past_the_real_recipe_count() {
        // Only 5 real recipes: cell index 5 exists geometrically but must not
        // be clickable — the partially-filled-row case.
        let r = grid_rect(5, 0).unwrap();
        assert_eq!(hit_test_local(5, 0, r.x + 1.0, r.y + 1.0), None);
        assert_eq!(hit_test_local(6, 0, r.x + 1.0, r.y + 1.0), Some(5));
    }

    #[test]
    fn offscreen_rows_and_scroll_start_index_match_the_transcribed_formula() {
        // 12 or fewer recipes: nothing to scroll.
        assert_eq!(offscreen_rows(12), 0);
        assert_eq!(start_index_for_scroll(1.0, 12), 0);
        // 20 recipes -> 5 rows total -> 2 offscreen rows.
        assert_eq!(offscreen_rows(20), 2);
        assert_eq!(start_index_for_scroll(0.0, 20), 0);
        // (1.0 * 2 + 0.5) as i32 = 2, * 4 = 8.
        assert_eq!(start_index_for_scroll(1.0, 20), 8);
        // scroll_offset outside 0..=1 is clamped first.
        assert_eq!(start_index_for_scroll(5.0, 20), start_index_for_scroll(1.0, 20));
        assert_eq!(start_index_for_scroll(-5.0, 20), start_index_for_scroll(0.0, 20));
    }
}
