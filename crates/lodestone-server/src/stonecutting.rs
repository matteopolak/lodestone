//! The stonecutter (station half): a real, server-computed
//! recipe list from the stonecutting corpus [`crate::crafting::recipe_book`]
//! already loads — the real stonecutting menu's own recipes-for-input /
//! setup-result-slot rule.
//!
//! # What it is
//!
//! The real stonecutting menu keeps two pieces of state beyond the plain
//! grid: the recipe list for the current input (every stonecutting recipe
//! whose ingredient the current input item satisfies, recomputed whenever
//! the input's *item* changes) and the selected recipe index (set by the
//! menu-button click — [`crate::container_click`]'s `ContainerButtonClick`
//! consumer in `crate::server`). [`matches`] is the first; [`result`] folds
//! both into the one result the real result slot shows.
//!
//! # How it works
//!
//! [`matches`] filters `crate::crafting::recipe_book()`'s already-loaded
//! `Recipe::Stonecutting` entries by [`lodestone_game::recipe::Ingredient::matches`]
//! against the input item, resolving tags through the same
//! [`lodestone_game::recipe::TagResolver`] the crafting-book corpus already
//! uses (`recipe_book().tags()`) — no second ingredient table, no second tag
//! source.
//!
//! **Ordering is a disclosed approximation.** The real order comes from
//! the recipe manager's own registration order, which this crate does not
//! preserve per-recipe (`recipe_book()` is a `HashMap`-shaped corpus, per its
//! own doc). [`matches`] sorts by recipe id instead — stable across calls (so
//! a `ContainerButtonClick`'s `button_id` keeps meaning the same recipe
//! between one input change and the next) but not guaranteed to be the real
//! exact button order. Getting the *exact* order would need a JVM oracle
//! dump the same way a dedicated oracle program exists for entity metadata
//! indices — nobody has built one for the real stonecutting registration
//! order yet.
//!
//! # How to change it
//!
//! Nothing here is hand-maintained: every stonecutting recipe already loads
//! through [`crate::crafting::recipe_book`] from the real datapack JSON. A
//! new stonecutting recipe needs no change in this module at all.
//!
//! # Dependencies
//!
//! [`crate::crafting::recipe_book`] for the corpus and its tag resolver.

use lodestone_model::ItemStack;

/// Every stonecutting result `input` can produce, in a stable
/// (recipe-id-sorted — see this module's own doc for why that is not
/// necessarily the real exact order) sequence — the real recipes-for-input
/// rule.
#[must_use]
pub fn matches(input: &ItemStack) -> Vec<ItemStack> {
    use lodestone_game::recipe::Recipe;

    let book = crate::crafting::recipe_book();
    let tags = book.tags();
    let mut entries: Vec<(&lodestone_model::Identifier, &lodestone_game::item::ItemStack)> = book
        .iter()
        .filter_map(|(id, recipe)| match recipe {
            Recipe::Stonecutting { ingredient, result } if ingredient.matches(&input.item, tags) => {
                Some((id, result))
            }
            _ => None,
        })
        .collect();
    entries.sort_by_key(|(id, _)| *id);
    // `lodestone_game::item::ItemStack` and `lodestone_model::ItemStack` are two
    // distinct types (`crate::crafting::derive_result`'s own doc comment gives the
    // full reason) — every stonecutting result crosses that seam the same way.
    entries
        .into_iter()
        .map(|(_, result)| ItemStack::new(result.item().clone(), result.count().max(0).unsigned_abs()))
        .collect()
}

/// How many offers [`matches`] would return for `input` — `crate::server`'s
/// `ContainerButtonClick` consumer's own validity check, without needing the
/// whole list. `0` for no input, matching the real visible-recipe-count rule
/// on an empty input slot.
#[must_use]
pub fn count(input: Option<&ItemStack>) -> usize {
    input.map_or(0, |item| matches(item).len())
}

/// The result slot for one stonecutter menu: `input`'s recipe list at
/// `selected`, or `None` if either is missing or `selected` is out of range
/// — the real setup-result-slot rule's own valid-recipe-index guard, folded
/// into one call for [`crate::server::workstation_result`]'s recipe closure.
#[must_use]
pub fn result(input: Option<&ItemStack>, selected: Option<i32>) -> Option<ItemStack> {
    let input = input?;
    let selected = usize::try_from(selected?).ok()?;
    matches(input).into_iter().nth(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str) -> ItemStack {
        ItemStack::new(item.parse().expect("valid key"), 1)
    }

    /// The headline case: cobblestone cuts into several real stonecutter
    /// outputs (stairs, slabs, walls at minimum) — proving the corpus is
    /// actually reached, not an empty list.
    #[test]
    fn cobblestone_has_multiple_real_stonecutting_outputs() {
        let outputs = matches(&stack("minecraft:cobblestone"));
        assert!(
            outputs.len() >= 3,
            "cobblestone must cut into several real outputs, got {outputs:?}"
        );
        let names: Vec<String> = outputs.iter().map(|s| s.item.to_string()).collect();
        assert!(names.contains(&"minecraft:cobblestone_stairs".to_string()), "{names:?}");
        assert!(names.contains(&"minecraft:cobblestone_slab".to_string()), "{names:?}");
        assert!(names.contains(&"minecraft:cobblestone_wall".to_string()), "{names:?}");
    }

    /// An item with no stonecutting recipe at all — the discriminating
    /// control against "matches returns everything".
    #[test]
    fn an_uncuttable_item_has_no_matches() {
        assert!(matches(&stack("minecraft:dirt")).is_empty());
    }

    /// `result` picks the exact recipe at `selected`, not merely "some"
    /// result — asserted against the same list `matches` returns, so a
    /// transposed index would fail here even though both are internally
    /// consistent with each other.
    #[test]
    fn result_picks_the_exact_selected_index() {
        let input = stack("minecraft:cobblestone");
        let outputs = matches(&input);
        assert!(outputs.len() >= 2, "need at least two outputs to distinguish an index: {outputs:?}");
        assert_eq!(result(Some(&input), Some(0)), Some(outputs[0].clone()));
        assert_eq!(result(Some(&input), Some(1)), Some(outputs[1].clone()));
    }

    /// Out-of-range, missing, or negative selection all fall back to no
    /// result — `isValidRecipeIndex`'s guard.
    #[test]
    fn an_invalid_selection_has_no_result() {
        let input = stack("minecraft:cobblestone");
        let count = matches(&input).len() as i32;
        assert_eq!(result(Some(&input), Some(-1)), None);
        assert_eq!(result(Some(&input), Some(count + 100)), None);
        assert_eq!(result(Some(&input), None), None);
        assert_eq!(result(None, Some(0)), None);
    }
}
