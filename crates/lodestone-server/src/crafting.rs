//! Server-authoritative crafting (issue #529).
//!
//! ## What it is
//!
//! The crafting grid the server owns, plus the corpus it resolves a result
//! against. Before this module the server had **no crafting concept at all**:
//! [`crate::inventory::PlayerInventory`] dropped menu slots `0..=4`, no menu
//! opened for a crafting table, `PLACE_RECIPE` decoded into nothing, and
//! `apply_container_clicked` applied whatever slot diff the client claimed — so a
//! client could mint any item by sending a container diff naming it.
//!
//! ## How it works
//!
//! [`CraftingState`] is vanilla's `CraftingContainer` + `ResultContainer` pair:
//! `width * height` input cells and one result slot. Every mutation of an input
//! cell goes through [`CraftingState::set_input`], which immediately re-derives
//! the result from [`recipe_book`] — the same `RecipeBook` matcher the *client*
//! uses for its prediction, deliberately not a second one. So the result slot is
//! never written by anything the client sent; a claimed result is only ever
//! *compared* against ours, by [`CraftingState::claim_matches`].
//!
//! The corpus is bundled and embedded (`assets/recipe/`, `assets/tags/item/`, via
//! `build.rs`), following the `assets/loot_table/` precedent, because the client
//! reads its own corpus out of `client.jar` through `lodestone-assets` and this
//! crate cannot depend on that. **The corpus must be complete or absent** — a
//! partial one rejects *valid* crafts, which is a worse failure than the trust it
//! replaces, so [`recipe_book`] is built once from every bundled file and
//! [`BUNDLED_CRAFTING_RECIPES`] pins the count.
//!
//! ## How to change it
//!
//! To refresh the corpus, re-copy `crafting_shaped` + `crafting_shapeless` from
//! `.cache/mc/26.2/src/data/minecraft/recipe/` and all of
//! `data/minecraft/tags/item/`, then update [`BUNDLED_CRAFTING_RECIPES`]. Both
//! halves or neither: an ingredient spelled `#minecraft:planks` matches nothing
//! without its tag document.
//!
//! Grid *layout* (which menu slot is which cell) is [`crate::inventory`]'s
//! business, not this module's — see `crafting_menu_slot`.
//!
//! ## Dependencies
//!
//! `lodestone-game`'s `recipe` + `recipe_json` (its `json` feature). That crate
//! depends on `lodestone-model` and `uuid` only, so this adds no protocol or
//! client coupling — see this crate's `Cargo.toml` for why the earlier "keep
//! `lodestone-game` out" note was revisited.

use std::sync::OnceLock;

use lodestone_game::recipe::{CraftingGrid, RecipeBook};
use lodestone_game::recipe_json::CorpusBuilder;
use lodestone_model::ItemStack;

include!(concat!(env!("OUT_DIR"), "/embedded_embedded_recipes.rs"));
include!(concat!(env!("OUT_DIR"), "/embedded_embedded_item_tags.rs"));

/// Number of bundled grid recipes — vanilla 26.2's full `crafting_shaped` (733)
/// plus `crafting_shapeless` (323) set.
///
/// Pinned as a constant rather than left implicit because a corpus that silently
/// lost files is the failure mode that matters here: it rejects valid crafts,
/// and every individual recipe still works.
pub const BUNDLED_CRAFTING_RECIPES: usize = 1056;

/// The process-wide crafting corpus, parsed once.
///
/// Deliberately a `OnceLock` and not a per-connection field: it is ~1,000
/// immutable recipes plus 224 tags, identical for every player, and parsing it
/// per join would be the whole cost paid per connection.
pub fn recipe_book() -> &'static RecipeBook {
    static BOOK: OnceLock<RecipeBook> = OnceLock::new();
    BOOK.get_or_init(|| {
        let mut builder = CorpusBuilder::new();
        for (id, raw) in EMBEDDED_ITEM_TAGS {
            if let Ok(key) = format!("minecraft:{id}").parse() {
                builder.push_tag(key, raw);
            }
        }
        for (id, raw) in EMBEDDED_RECIPES {
            if let Ok(key) = format!("minecraft:{id}").parse() {
                builder.push_recipe(key, raw);
            }
        }
        builder.finish()
    })
}

/// The server's own crafting grid and the result *it* computed.
///
/// One per open crafting menu. The player inventory screen's 2×2 and a crafting
/// table's 3×3 are the same type at different dimensions, exactly as vanilla's
/// `CraftingContainer` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CraftingState {
    width: usize,
    height: usize,
    inputs: Vec<Option<ItemStack>>,
    result: Option<ItemStack>,
}

impl CraftingState {
    /// A 2×2 grid — the player inventory screen's own (`InventoryMenu`).
    #[must_use]
    pub fn player() -> Self {
        Self::new(2, 2)
    }

    /// A 3×3 grid — a crafting table's (`CraftingMenu`).
    #[must_use]
    pub fn table() -> Self {
        Self::new(3, 3)
    }

    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            inputs: vec![None; width * height],
            result: None,
        }
    }

    /// Grid width in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Number of input cells.
    #[must_use]
    pub fn input_count(&self) -> usize {
        self.inputs.len()
    }

    /// One input cell.
    #[must_use]
    pub fn input(&self, index: usize) -> Option<&ItemStack> {
        self.inputs.get(index).and_then(Option::as_ref)
    }

    /// The result **the server derived**. Never written from the wire.
    #[must_use]
    pub fn result(&self) -> Option<&ItemStack> {
        self.result.as_ref()
    }

    /// Whether every input cell is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inputs.iter().all(Option::is_none)
    }

    /// Write one input cell and re-derive the result. Returns whether the cell
    /// was in range.
    ///
    /// The re-derivation is *inside* the setter on purpose: a caller that could
    /// mutate an input without recomputing would leave a stale result slot, and
    /// a stale result is exactly the thing the client is otherwise trusted for.
    pub fn set_input(&mut self, index: usize, item: Option<ItemStack>) -> bool {
        if index >= self.inputs.len() {
            return false;
        }
        self.inputs[index] = item;
        self.recompute();
        true
    }

    /// Empty every input cell and the result — vanilla's behaviour on closing a
    /// crafting menu (the grid's contents are returned to the player, and a
    /// closed menu keeps nothing).
    pub fn clear(&mut self) {
        for cell in &mut self.inputs {
            *cell = None;
        }
        self.result = None;
    }

    /// Consume one craft: decrement every occupied input by one and re-derive.
    /// Mirrors vanilla's `ItemStack.shrink(1)` per grid slot in
    /// `ResultSlot.onTake`.
    pub fn consume_one(&mut self) {
        for cell in &mut self.inputs {
            if let Some(stack) = cell {
                if stack.count <= 1 {
                    *cell = None;
                } else {
                    stack.count -= 1;
                }
            }
        }
        self.recompute();
    }

    /// Whether a *claimed* result stack agrees with the one this grid actually
    /// produces — the check that closes the mint-anything hole.
    ///
    /// Item **and** count, because a recipe's yield is part of the recipe: a
    /// claim of 64 sticks from one plank has the right item.
    #[must_use]
    pub fn claim_matches(&self, claimed: Option<&ItemStack>) -> bool {
        match (self.result.as_ref(), claimed) {
            (None, None) => true,
            (Some(ours), Some(theirs)) => ours.item == theirs.item && ours.count == theirs.count,
            _ => false,
        }
    }

    fn recompute(&mut self) {
        let cells = self
            .inputs
            .iter()
            .map(|slot| slot.as_ref().map(|stack| stack.item.clone()))
            .collect();
        let grid = CraftingGrid::new(self.width, self.height, cells);
        self.result = if grid.is_empty() {
            None
        } else {
            recipe_book().match_grid(&grid).map(|result| {
                // `lodestone_game::item::ItemStack` and `lodestone_model::ItemStack`
                // are two distinct types (a signed working count vs. an unsigned
                // stored one); the recipe corpus speaks the former and every slot
                // in this crate the latter.
                ItemStack::new(result.item().clone(), result.count().max(0).unsigned_abs())
            })
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str, count: u32) -> ItemStack {
        ItemStack::new(name.parse().expect("valid key"), count)
    }

    /// The corpus is complete. A short corpus rejects valid crafts and every
    /// recipe that *is* present still works, so nothing else here would notice.
    #[test]
    fn the_bundled_corpus_is_complete() {
        let book = recipe_book();
        assert_eq!(EMBEDDED_RECIPES.len(), BUNDLED_CRAFTING_RECIPES);
        assert_eq!(book.len(), BUNDLED_CRAFTING_RECIPES);
        assert_eq!(EMBEDDED_ITEM_TAGS.len(), 224);
    }

    /// A real shaped recipe, with the expected result read from vanilla's own
    /// datapack rather than from our matcher: `crafting_table.json` is four
    /// `#minecraft:planks` in a 2×2 producing one `minecraft:crafting_table`.
    /// Both the shape and the tag resolution have to work for this to pass.
    #[test]
    fn a_2x2_shaped_recipe_resolves_through_an_item_tag() {
        let mut grid = CraftingState::player();
        for i in 0..4 {
            assert!(grid.set_input(i, Some(stack("minecraft:oak_planks", 3))));
        }
        assert_eq!(
            grid.result().map(|r| (r.item.to_string(), r.count)),
            Some(("minecraft:crafting_table".to_string(), 1))
        );
    }

    /// The 3×3 arm, and shape sensitivity with the orientation as the *only*
    /// difference: `stick.json` is `["#","#"]` (two planks in a column, yielding
    /// 4) and `oak_pressure_plate.json` is `["##"]` (the same two side by side,
    /// yielding 1). A matcher that ignored the shape would return one of them for
    /// both, and a diagonal — no vanilla recipe — must return neither.
    #[test]
    fn a_shaped_recipe_respects_its_shape() {
        let mut column = CraftingState::table();
        column.set_input(0, Some(stack("minecraft:oak_planks", 1)));
        column.set_input(3, Some(stack("minecraft:oak_planks", 1)));
        assert_eq!(
            column.result().map(|r| (r.item.to_string(), r.count)),
            Some(("minecraft:stick".to_string(), 4))
        );

        let mut row = CraftingState::table();
        row.set_input(0, Some(stack("minecraft:oak_planks", 1)));
        row.set_input(1, Some(stack("minecraft:oak_planks", 1)));
        assert_eq!(
            row.result().map(|r| (r.item.to_string(), r.count)),
            Some(("minecraft:oak_pressure_plate".to_string(), 1))
        );

        let mut diagonal = CraftingState::table();
        diagonal.set_input(0, Some(stack("minecraft:oak_planks", 1)));
        diagonal.set_input(4, Some(stack("minecraft:oak_planks", 1)));
        assert!(diagonal.result().is_none());
    }

    /// The security property: a claim the grid does not produce is refused, and
    /// consuming a craft shrinks the inputs and re-derives.
    #[test]
    fn a_claimed_result_the_grid_cannot_produce_is_refused() {
        let mut grid = CraftingState::player();
        assert!(
            grid.claim_matches(None),
            "an empty grid claiming nothing is fine"
        );
        assert!(
            !grid.claim_matches(Some(&stack("minecraft:diamond_block", 1))),
            "an empty grid cannot produce a diamond block"
        );

        for i in 0..4 {
            grid.set_input(i, Some(stack("minecraft:oak_planks", 2)));
        }
        assert!(grid.claim_matches(Some(&stack("minecraft:crafting_table", 1))));
        assert!(
            !grid.claim_matches(Some(&stack("minecraft:crafting_table", 64))),
            "the count is part of the recipe"
        );

        grid.consume_one();
        for i in 0..4 {
            assert_eq!(grid.input(i).map(|s| s.count), Some(1));
        }
        grid.consume_one();
        assert!(grid.is_empty());
        assert!(grid.result().is_none());
    }
}
