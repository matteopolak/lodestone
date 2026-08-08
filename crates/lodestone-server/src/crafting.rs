//! Server-authoritative crafting (issue #529).
//!
//! ## What it is
//!
//! The crafting grid the server owns, plus the corpus it resolves a result
//! against. Before this module the server had **no crafting concept at all**:
//! [`crate::inventory::PlayerInventory`] dropped menu slots `0..=4` and
//! `apply_container_clicked` applied whatever slot diff the client claimed,
//! including the result slot — so a container diff could name any item as a
//! crafting output and the server would store it.
//!
//! **What is still not here**: a crafting-*table* menu (nothing opens one, so the
//! 3×3 [`CraftingState::table`] has no production caller yet) and
//! `PLACE_RECIPE`. Both are steps 2 and 4 of issue #529's scope and are still
//! open on it.
//!
//! ## How it works
//!
//! [`CraftingState`] is vanilla's `CraftingContainer` + `ResultContainer` pair:
//! `width * height` input cells and one result slot. Every mutation of an input
//! cell goes through [`CraftingState::set_input`], which immediately re-derives
//! the result from [`recipe_book`] — the same `RecipeBook` matcher the *client*
//! uses for its prediction, deliberately not a second one. So the result slot is
//! never written by anything the client sent — a claimed result is dropped and
//! the server's own value pushed back in its place.
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
//! business, not this module's — see `player_craft_grid_cell`.
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

/// The recipe at `index` in the bundled corpus's id-sorted order — the id space a
/// `PLACE_RECIPE` packet's `RecipeDisplayId` refers to.
///
/// **`RecipeDisplayId` is an opaque index the *server* assigns**, not a name:
/// vanilla hands the client its whole book with `ClientboundRecipeBookAddPacket`
/// and the client echoes back a position in that list. This crate does not encode
/// that packet yet, so it defines the index as the corpus's own sorted order —
/// which is exactly the order such an encoder would emit, so the two cannot
/// disagree once it exists.
///
/// The consequence is worth stating: until `recipe_book_add` is encoded, **our own
/// client never learns any id and so never sends a valid `PLACE_RECIPE`.** A real
/// vanilla 26.2 client is in the same position. So this half is complete and the
/// packet is currently unreachable; see `docs/server-side-crafting.md`.
#[must_use]
pub fn recipe_at_index(index: usize) -> Option<(&'static lodestone_model::Identifier, &'static lodestone_game::recipe::Recipe)> {
    recipe_book().iter().nth(index)
}

/// Fills `grid` with `recipe`'s ingredients, taken out of `inventory` — vanilla's
/// `ServerPlaceRecipe` (issue #529 step 4, the `PLACE_RECIPE` consumer).
///
/// Whatever was in the grid goes back to the player first (vanilla's
/// `clearGrid`), then one item per pattern cell is moved in. `use_max_items` runs
/// as many rounds as the inventory and the per-item stack cap allow, which is
/// vanilla's shift-click-a-recipe behaviour.
///
/// Returns `false` when the recipe has no placement for this grid's dimensions (a
/// 3×3 recipe asked for on the player screen's 2×2), in which case nothing moved.
pub fn place_recipe(
    inventory: &mut crate::inventory::PlayerInventory,
    grid: &mut CraftingState,
    recipe: &lodestone_game::recipe::Recipe,
    use_max_items: bool,
) -> bool {
    let (width, height) = (grid.width(), grid.height());
    let Some(placement) = recipe.placement(width, height) else {
        return false;
    };

    // The grid's current contents go back to the player before anything is taken.
    for cell in 0..width * height {
        if let Some(existing) = grid.input(cell).cloned() {
            grid.set_input(cell, None);
            inventory.add(existing);
        }
    }

    let tags = recipe_book().tags();
    let rounds = if use_max_items { 64 } else { 1 };
    let mut placed_any = false;
    for _ in 0..rounds {
        // A round is all-or-nothing: a partially-filled extra round would leave the
        // grid holding a shape that matches no recipe.
        let mut taken: Vec<(usize, ItemStack)> = Vec::new();
        let mut ok = true;
        for (cell, ingredient) in placement.iter().enumerate() {
            let Some(ingredient) = ingredient else { continue };
            let held = grid.input(cell).map_or(0, |s| s.count);
            if held >= 64 {
                ok = false;
                break;
            }
            match inventory.take_matching(|item| ingredient.matches(&item.item, tags)) {
                Some(stack) => taken.push((cell, stack)),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            // Give back whatever this failed round pulled out.
            for (_, stack) in taken {
                inventory.add(stack);
            }
            break;
        }
        for (cell, stack) in taken {
            let mut merged = grid.input(cell).cloned().unwrap_or_else(|| {
                let mut fresh = stack.clone();
                fresh.count = 0;
                fresh
            });
            merged.count += stack.count;
            grid.set_input(cell, Some(merged));
        }
        placed_any = true;
    }
    placed_any
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

    /// One input cell, or `None` for an empty or out-of-range one.
    #[must_use]
    pub fn input(&self, index: usize) -> Option<&ItemStack> {
        self.inputs.get(index).and_then(Option::as_ref)
    }

    /// Every input cell in row-major order — what a menu close returns to the
    /// player, and what `PLACE_RECIPE` overwrites.
    #[must_use]
    pub fn inputs(&self) -> &[Option<ItemStack>] {
        &self.inputs
    }

    /// Grid width, in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Grid height, in cells.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
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

    /// A recipe-book fill takes the ingredients out of the inventory and leaves a
    /// grid the server's own matcher then resolves — the `PLACE_RECIPE` consumer.
    ///
    /// Expected values from vanilla's `crafting_table.json`: four `#minecraft:planks`
    /// in a 2×2. So four planks leave the inventory, four cells hold one each, and
    /// the derived result is a crafting table.
    #[test]
    fn placing_a_recipe_fills_the_grid_from_the_inventory() {
        let (_, recipe) = recipe_book()
            .iter()
            .find(|(id, _)| id.path() == "crafting_table")
            .expect("crafting_table is bundled");

        let mut inventory = crate::inventory::PlayerInventory::new();
        inventory.set_native(0, Some(stack("minecraft:oak_planks", 10)));
        let mut grid = CraftingState::player();

        assert!(place_recipe(&mut inventory, &mut grid, recipe, false));
        for cell in 0..4 {
            assert_eq!(
                grid.input(cell).map(|s| s.count),
                Some(1),
                "cell {cell} should hold one plank"
            );
        }
        assert_eq!(inventory.native(0).map(|s| s.count), Some(6));
        assert_eq!(
            grid.result().map(|r| r.item.to_string()),
            Some("minecraft:crafting_table".to_string())
        );
    }

    /// A 3×3-only recipe has no placement on the player screen's 2×2, and a refused
    /// placement must leave the inventory untouched rather than half-consume it.
    #[test]
    fn a_3x3_recipe_is_refused_on_the_2x2_grid() {
        let (_, chest) = recipe_book()
            .iter()
            .find(|(id, _)| id.path() == "chest")
            .expect("chest is bundled");

        let mut inventory = crate::inventory::PlayerInventory::new();
        inventory.set_native(0, Some(stack("minecraft:oak_planks", 32)));
        let mut small = CraftingState::player();
        assert!(!place_recipe(&mut inventory, &mut small, chest, false));
        assert_eq!(inventory.native(0).map(|s| s.count), Some(32));
        assert!(small.is_empty());

        // …and it does fit the table's 3x3, which is the control that the refusal
        // above is about the dimensions and not about the recipe.
        let mut table = CraftingState::table();
        assert!(place_recipe(&mut inventory, &mut table, chest, false));
        assert_eq!(
            table.result().map(|r| r.item.to_string()),
            Some("minecraft:chest".to_string())
        );
    }

    /// The result tracks the grid in both directions: emptying a cell must
    /// *withdraw* a result that was there, not leave it standing. A stale result
    /// is the same defect as a trusted one.
    #[test]
    fn clearing_a_cell_withdraws_the_result() {
        let mut grid = CraftingState::player();
        for i in 0..4 {
            grid.set_input(i, Some(stack("minecraft:oak_planks", 1)));
        }
        assert!(grid.result().is_some());
        grid.set_input(3, None);
        assert!(grid.result().is_none());
        grid.clear();
        assert!(grid.is_empty());
        assert!(grid.result().is_none());
    }
}
