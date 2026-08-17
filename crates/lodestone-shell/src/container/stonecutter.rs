//! The stonecutter's recipe-selection scroll list (`ClientAction::
//! ContainerButtonClick`'s remainder for this screen — see
//! [`super::enchant`]'s module doc for the enchanting table's own producer,
//! the precedent this module follows).
//!
//! ## What it is
//!
//! `StonecutterScreen`/`StonecutterMenu` (`26.2`): a 4×3 grid of up to twelve
//! visible recipe buttons, scrollable when the input item matches more than
//! twelve stonecutting recipes. [`matches`] is the client-side mirror of
//! `lodestone-server`'s own `crate::stonecutting::matches` — the server half
//! landed first and computes this authoritatively for real; this is the
//! *client's* copy of the identical computation, which is what lets the grid
//! show real icons and lets a click be pre-validated before it is even sent,
//! exactly as vanilla's own client-side `StonecutterMenu` mirror does.
//!
//! ## How it works
//!
//! [`matches`] filters the shell's own loaded
//! [`lodestone_game::recipe::RecipeBook`] (`crate::resources::load_recipe_book`,
//! already adopted for the crafting recipe book — see `app.rs`'s
//! `recipe_book` field) for `Recipe::Stonecutting` entries whose ingredient
//! the input item satisfies, sorted by recipe id for the same "stable but not
//! necessarily vanilla's exact order" reason the server module's own doc
//! gives (no per-recipe registration order is preserved on this side of the
//! wire either).
//!
//! [`grid_rect`] is `StonecutterScreen`'s own real layout constants
//! (`RECIPES_X = 52`, `RECIPES_Y = 14`, a 16×18 cell, 4 columns) —
//! [`hit_test_local`] mirrors `StonecutterScreen.mouseClicked`'s exact
//! arithmetic, `start_index`-relative, and [`button_hit_test`] adds the panel
//! origin/scale resolution every click surface in this crate goes through.
//!
//! ## How to change it
//!
//! Nothing here is hand-maintained: every stonecutting recipe already loads
//! through the same jar-sourced `RecipeBook` the crafting recipe book uses.
//! Scrolling ([`start_index_for_scroll`]/[`scroll_offset_after_wheel`]) is
//! now wired to the mouse wheel — **stale, corrected**: this doc used to say
//! the scroll formula existed with nothing feeding it, which was true when
//! written and is not any more. `WindowApp::scroll_stonecutter`
//! (`app/container_input.rs`) computes a new offset per wheel notch and
//! persists it on `WindowApp::stonecutter_scroll`, and
//! `WindowApp::handle_stonecutter_click` reads that persisted offset through
//! [`start_index_for_scroll`] rather than pinning `start_index` at `0`. The
//! scrollbar thumb drag is still not wired — the same disclosed cut
//! [`super::loom`]'s own module doc makes for its scrollbar: the wheel alone
//! already reaches every match past twelve.
//!
//! ## Dependencies
//!
//! [`lodestone_game::recipe`] for the corpus/ingredient matching,
//! [`super::layout`] for the panel origin/scale seam every other click
//! surface in this crate resolves a cursor through.

use lodestone_game::item::ItemStack;
use lodestone_game::menu::{Menu, SpecialLayout};
use lodestone_game::recipe::{Recipe, RecipeBook};
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

/// `crate::stonecutting::matches` (`lodestone-server`), ported to the client:
/// every stonecutting result `input` can produce, in a stable (recipe-id
/// sorted) sequence — `StonecutterMenu.recipesForInput`. Empty for an empty
/// input, or one no stonecutting recipe accepts, matching
/// `StonecutterMenu.hasInputItem`/`getNumberOfVisibleRecipes() == 0`.
#[must_use]
pub fn matches(book: &RecipeBook, input: &Identifier) -> Vec<ItemStack> {
    let tags = book.tags();
    let mut entries: Vec<(&Identifier, &ItemStack)> = book
        .iter()
        .filter_map(|(id, recipe)| match recipe {
            Recipe::Stonecutting { ingredient, result } if ingredient.matches(input, tags) => Some((id, result)),
            _ => None,
        })
        .collect();
    entries.sort_by_key(|(id, _)| *id);
    entries.into_iter().map(|(_, result)| result.clone()).collect()
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
    use lodestone_game::recipe::Ingredient;

    fn id(s: &str) -> Identifier {
        s.parse().unwrap()
    }

    fn book_with_stone_recipes() -> RecipeBook {
        let mut book = RecipeBook::new();
        book.insert(
            id("minecraft:stone_stairs_from_stonecutting"),
            Recipe::Stonecutting {
                ingredient: Ingredient::Item(id("minecraft:stone")),
                result: ItemStack::new(id("minecraft:stone_stairs"), 1),
            },
        );
        book.insert(
            id("minecraft:stone_slab_from_stonecutting"),
            Recipe::Stonecutting {
                ingredient: Ingredient::Item(id("minecraft:stone")),
                result: ItemStack::new(id("minecraft:stone_slab"), 2),
            },
        );
        book.insert(
            id("minecraft:andesite_wall"),
            Recipe::Stonecutting {
                ingredient: Ingredient::Item(id("minecraft:andesite")),
                result: ItemStack::new(id("minecraft:andesite_wall"), 1),
            },
        );
        book
    }

    #[test]
    fn matches_filters_by_ingredient_and_sorts_by_id() {
        let book = book_with_stone_recipes();
        let results = matches(&book, &id("minecraft:stone"));
        assert_eq!(results.len(), 2, "andesite's recipe must not appear for a stone input");
        // "slab" < "stairs" lexicographically, so id order puts slab first.
        assert_eq!(results[0].item().path(), "stone_slab");
        assert_eq!(results[1].item().path(), "stone_stairs");
    }

    #[test]
    fn matches_is_empty_for_an_input_with_no_recipe() {
        let book = book_with_stone_recipes();
        assert!(matches(&book, &id("minecraft:dirt")).is_empty());
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
