//! Server-authoritative crafting.
//!
//! ## What it is
//!
//! The crafting grid the server owns, plus the corpus it resolves a result
//! against. [`crate::inventory::PlayerInventory`] exposes menu slots `0..=4`,
//! and `apply_container_clicked` treats the client slot diff as a claim rather
//! than authority, including for the result slot. The server derives the
//! output from the authoritative grid and recipe corpus.
//!
//! **What is still not here**: a crafting-*table* menu — no production path opens one, so the
//! 3×3 [`CraftingState::table`] has no production caller yet. `PLACE_RECIPE` is
//! implemented: [`recipe_book_entries`] supplies opaque `RecipeDisplayId`
//! values, and the join path sends the complete recipe book that those values
//! index.
//!
//! ## How it works
//!
//! [`CraftingState`] owns `width * height` input cells and one result slot. Every
//! input mutation goes through [`CraftingState::set_input`], which immediately
//! re-derives the result from [`recipe_book`]. The same corpus supports client
//! prediction and server authority, so the result slot is never written by
//! anything the client sent — a claimed result is dropped and the server's own
//! value pushed back in its place.
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
//! To refresh the corpus, re-copy `crafting_shaped` + `crafting_shapeless` +
//! `stonecutting` (the bundled corpus includes the third — [`crate::stonecutting`] reads
//! it back out of this same corpus, rather than a second bundle) from
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

/// Number of bundled recipe JSON files — vanilla 26.2's full `crafting_shaped`
/// (733) plus `crafting_shapeless` (323) set, plus the full `stonecutting` set
/// (319) — 1,375 total. All three
/// live in the same `assets/recipe/` directory and the same
/// [`EMBEDDED_RECIPES`] table; only the JSON's own `"type"` field
/// distinguishes them, so no second bundling mechanism was needed to add the
/// stonecutting set.
///
/// Pinned as a constant rather than left implicit because a corpus that silently
/// lost files is the failure mode that matters here: it rejects valid crafts,
/// and every individual recipe still works.
pub const BUNDLED_CRAFTING_RECIPES: usize = 1375;

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
/// the server hands the client its whole book and the client echoes back a
/// position in that list. [`recipe_book_entries`] encodes that packet, walking
/// this same id-sorted order, so the two index
/// spaces are one by construction — and
/// `crates/protocol/v770/tests/recipe_book_add.rs`'s
/// `every_entry_id_resolves_to_the_same_recipe` asserts it, because a drift here
/// places a *different* recipe on every click, silently and plausibly.
#[must_use]
pub fn recipe_at_index(index: usize) -> Option<(&'static lodestone_model::Identifier, &'static lodestone_game::recipe::Recipe)> {
    recipe_book().iter().nth(index)
}

/// The slot-display variants that a crafting recipe can produce.
///
/// Slot displays are recursive and have eleven registered types; a shaped or
/// shapeless crafting recipe reaches exactly these five, because
/// `Ingredient.display()` yields either a `tag` or a `composite` of `item`s, a
/// result is an `item_stack`, an absent pattern cell is `empty`, and the crafting
/// station is an `item`. The other six (`any_fuel`, `with_any_potion`,
/// `only_with_component`, `dyed`, `smithing_trim`, `with_remainder`) belong to
/// furnace, brewing and smithing displays, which this corpus does not encode.
///
/// **The variant order below is not the wire order.** Registry ids come from
/// the protocol registry, and the encoder is the one place that knows them.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotDisplay {
    /// `empty` — an unfilled pattern cell.
    Empty,
    /// `item` — one item by id, no count and no components.
    Item(lodestone_model::Identifier),
    /// `item_stack` — an `ItemStackTemplate`: item id and count. Components are
    /// not carried because the bundled corpus's results have none; a recipe that
    /// grew one would need this widened, not worked around at the encoder.
    Stack {
        /// The result item id.
        item: lodestone_model::Identifier,
        /// The result count.
        count: i32,
    },
    /// `tag` — an item tag the client resolves itself.
    Tag(lodestone_model::Identifier),
    /// `composite` — any of the contained displays.
    Composite(Vec<SlotDisplay>),
}

impl SlotDisplay {
    /// Vanilla `Ingredient.display()`: a tag stays a tag (so the client shows the
    /// whole cycling set), and an explicit list becomes a composite of items.
    #[must_use]
    fn of_ingredient(ingredient: &lodestone_game::recipe::Ingredient) -> Self {
        use lodestone_game::recipe::Ingredient;
        match ingredient {
            Ingredient::Item(id) => Self::Item(id.clone()),
            Ingredient::Tag(tag) => Self::Tag(tag.clone()),
            Ingredient::Any(options) => {
                Self::Composite(options.iter().map(Self::of_ingredient).collect())
            }
        }
    }
}

/// Vanilla's `RecipeDisplay`, restricted to the two crafting types.
///
/// `crafting_station` is `minecraft:crafting_table` for both, exactly as
/// `ShapedRecipe.display()`/`ShapelessRecipe.display()` hardcode it.
#[derive(Debug, Clone, PartialEq)]
pub enum RecipeDisplay {
    /// `crafting_shaped`. `ingredients.len()` **must** equal `width * height` —
    /// vanilla's own record throws otherwise, and a real client's reader will
    /// disagree about where the list ends if it does not.
    Shaped {
        /// Pattern width, not the grid's.
        width: i32,
        /// Pattern height, not the grid's.
        height: i32,
        /// Row-major, `width * height` entries.
        ingredients: Vec<SlotDisplay>,
        /// The output.
        result: SlotDisplay,
    },
    /// `crafting_shapeless`.
    Shapeless {
        /// In declaration order.
        ingredients: Vec<SlotDisplay>,
        /// The output.
        result: SlotDisplay,
    },
}

/// One `ClientboundRecipeBookAddPacket.Entry` — a `RecipeDisplayEntry` plus its
/// notification/highlight flag byte.
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeBookEntry {
    /// `RecipeDisplayId.index` — the position this entry occupies, which is what
    /// a later `PLACE_RECIPE` echoes back. **The same index
    /// [`recipe_at_index`] resolves**, by construction: both walk
    /// [`recipe_book`]'s id-sorted order.
    pub id: i32,
    /// The display the client draws and lays out from.
    pub display: RecipeDisplay,
    /// `OptionalInt group` — vanilla's numeric group id for the book's
    /// "alternatives" cycling. `None` for a recipe with no `group`.
    pub group: Option<i32>,
    /// `minecraft:recipe_book_category` id (e.g. `crafting_misc`).
    pub category: &'static str,
    /// `Optional<List<Ingredient>>` — what the client's own "can I craft this"
    /// highlight uses. Each entry is one ingredient's flattened item set; an
    /// entry naming a tag is left for the encoder to resolve, so this is the
    /// resolved item list.
    pub crafting_requirements: Vec<Vec<lodestone_model::Identifier>>,
}

impl RecipeBookEntry {
    /// The entry's result display, whichever display type it carries.
    #[must_use]
    pub fn display_result(&self) -> &SlotDisplay {
        match &self.display {
            RecipeDisplay::Shaped { result, .. } | RecipeDisplay::Shapeless { result, .. } => {
                result
            }
        }
    }
}

/// The whole bundled crafting corpus as recipe-book entries, in the id-sorted
/// order [`recipe_at_index`] indexes.
///
/// Built once and cached, for the same reason [`recipe_book`] is: it is the same
/// ~1,000 immutable recipes for every player, and it is sent at every join.
///
/// Only grid recipes appear. A cooking/stonecutter/smithing recipe would need its
/// own `RecipeDisplay` type and its own book, and the corpus this crate bundles is
/// `crafting_shaped` + `crafting_shapeless` only.
pub fn recipe_book_entries() -> &'static [RecipeBookEntry] {
    static ENTRIES: OnceLock<Vec<RecipeBookEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        use lodestone_game::recipe::Recipe;
        let tags = recipe_book().tags();
        // Group ids are assigned in first-encounter order over the same sorted
        // walk, so they are stable for a given corpus without needing to be
        // stored anywhere.
        let mut groups: Vec<String> = Vec::new();
        let mut group_id = |name: Option<&str>| -> Option<i32> {
            let name = name?;
            let index = match groups.iter().position(|g| g == name) {
                Some(i) => i,
                None => {
                    groups.push(name.to_string());
                    groups.len() - 1
                }
            };
            i32::try_from(index).ok()
        };
        let requirements = |ingredients: Vec<&lodestone_game::recipe::Ingredient>| {
            ingredients
                .into_iter()
                .map(|ingredient| resolve_ingredient_items(ingredient, tags))
                .collect()
        };

        recipe_book()
            .iter()
            .enumerate()
            .filter_map(|(index, (_, recipe))| {
                let id = i32::try_from(index).ok()?;
                let (display, reqs, group) = match recipe {
                    Recipe::Shaped(shaped) => {
                        let ingredients: Vec<SlotDisplay> = shaped
                            .pattern()
                            .iter()
                            .map(|cell| {
                                cell.as_ref().map_or(SlotDisplay::Empty, |i| {
                                    SlotDisplay::of_ingredient(i)
                                })
                            })
                            .collect();
                        let reqs = requirements(shaped.pattern().iter().flatten().collect());
                        (
                            RecipeDisplay::Shaped {
                                width: i32::try_from(shaped.width()).unwrap_or(0),
                                height: i32::try_from(shaped.height()).unwrap_or(0),
                                ingredients,
                                result: SlotDisplay::Stack {
                                    item: shaped.result().item().clone(),
                                    count: shaped.result().count(),
                                },
                            },
                            reqs,
                            group_id(shaped.group()),
                        )
                    }
                    Recipe::Shapeless(shapeless) => {
                        let ingredients = shapeless
                            .ingredients()
                            .iter()
                            .map(SlotDisplay::of_ingredient)
                            .collect();
                        let reqs = requirements(shapeless.ingredients().iter().collect());
                        (
                            RecipeDisplay::Shapeless {
                                ingredients,
                                result: SlotDisplay::Stack {
                                    item: shapeless.result().item().clone(),
                                    count: shapeless.result().count(),
                                },
                            },
                            reqs,
                            group_id(shapeless.group()),
                        )
                    }
                    // Not a grid recipe: no crafting `RecipeDisplay` exists for it.
                    // The index is still consumed, so `recipe_at_index` and this
                    // list cannot drift.
                    _ => return None,
                };
                Some(RecipeBookEntry {
                    id,
                    display,
                    group,
                    category: book_category(recipe),
                    crafting_requirements: reqs,
                })
            })
            .collect()
    })
}

/// The `minecraft:recipe_book_category` id for a grid recipe —
/// vanilla's own recipe-book-categories registry lists four tabs for crafting.
fn book_category(recipe: &lodestone_game::recipe::Recipe) -> &'static str {
    use lodestone_game::recipe::RecipeCategory;
    match recipe.category() {
        Some(RecipeCategory::Building) => "crafting_building_blocks",
        Some(RecipeCategory::Redstone) => "crafting_redstone",
        Some(RecipeCategory::Equipment) => "crafting_equipment",
        _ => "crafting_misc",
    }
}

/// Flattens one ingredient to the item ids that satisfy it, resolving tags —
/// what `Ingredient.CONTENTS_STREAM_CODEC` puts on the wire (a `HolderSet<Item>`,
/// which for our purposes is always the explicit list form).
fn resolve_ingredient_items(
    ingredient: &lodestone_game::recipe::Ingredient,
    tags: &lodestone_game::recipe::TagResolver,
) -> Vec<lodestone_model::Identifier> {
    use lodestone_game::recipe::Ingredient;
    match ingredient {
        Ingredient::Item(id) => vec![id.clone()],
        Ingredient::Tag(tag) => {
            // Sorted, so the wire order is stable across runs — a `HashSet`'s
            // iteration order is not, and a per-join reshuffle of an ingredient's
            // item list is the sort of thing that shows up as a flaky byte gate.
            let mut items: Vec<_> = tags.resolve(tag).into_iter().collect();
            items.sort();
            items
        }
        Ingredient::Any(options) => options
            .iter()
            .flat_map(|o| resolve_ingredient_items(o, tags))
            .collect(),
    }
}

/// Whether `grid` currently holds **exactly** `placement`'s shape — every named
/// cell occupied by a matching item and every unnamed cell empty.
///
/// Vanilla's `RecipeBookMenu.recipeMatches`, and the whole of what decides between
/// "top this up" and "clear it and start over" in [`place_recipe`]. It has to be
/// an exact shape test in both directions: a grid holding a *superset* (the right
/// items plus junk in a cell this recipe does not use) is a different craft, and
/// topping it up would leave it matching nothing.
fn grid_matches(
    grid: &CraftingState,
    placement: &[Option<&lodestone_game::recipe::Ingredient>],
    cells: usize,
) -> bool {
    let tags = recipe_book().tags();
    (0..cells).all(|cell| {
        match (grid.input(cell), placement.get(cell).and_then(|slot| slot.as_ref())) {
            (Some(stack), Some(ingredient)) => {
                stack.count > 0 && ingredient.matches(&stack.item, tags)
            }
            (None, None) => true,
            // An occupied cell this recipe does not use, or an empty cell it does.
            (Some(stack), None) => stack.count <= 0,
            (None, Some(_)) => false,
        }
    })
}

/// Fills `grid` with `recipe`'s ingredients, taken out of `inventory` for a
/// `PLACE_RECIPE` request.
///
/// # Repeated clicks accumulate, and that is the whole behaviour
///
/// A fresh or different recipe clears the grid and places one craft. When the
/// grid already matches the recipe, placement adds one more craft; shift-click
/// placement takes as many ingredients as the inventory allows. The equivalent
/// craft amounts are `smallest_stack_size + 1` for a matching grid and `1` for a
/// fresh grid. So:
///
/// | click | grid already holds | result |
/// |---|---|---|
/// | plain | nothing, or a different recipe | clear the inputs, then **one** craft |
/// | plain | this same recipe | **one more** craft on top, grid not cleared |
/// | shift (`use_max_items`) | either | as many as the ingredients allow, across multiple source stacks |
///
/// This implementation reaches the accumulate case by not clearing and merging
/// one more round, which is observationally the same and cannot lose items to
/// `PlayerInventory::add`'s stack cap on the way
/// through. That matters: `add` caps every write at 64 regardless of the item's own
/// maximum (its own doc says so), so a round trip through the inventory is not
/// free, and the fewer of them a top-up performs the better.
///
/// **The grid is only cleared once a placement is actually going to happen.** A
/// click the player cannot afford must leave the grid alone rather than empty it into the
/// inventory — that is a visible change on a click that should have done nothing.
///
/// Returns `false` when the recipe has no placement for this grid's dimensions (a
/// 3×3 recipe asked for on the player screen's 2×2) or when nothing could be taken,
/// in which case nothing moved.
pub fn place_recipe(
    inventory: &mut crate::inventory::PlayerInventory,
    grid: &mut CraftingState,
    recipe: &lodestone_game::recipe::Recipe,
    use_max_items: bool,
) -> bool {
    let (width, height) = (grid.width(), grid.height());
    let cells = width * height;
    let Some(placement) = recipe.placement(width, height) else {
        return false;
    };

    // The fork. A grid already holding this recipe is topped up in place; anything
    // else is returned to the player first.
    let accumulating = grid_matches(grid, &placement, cells);

    let tags = recipe_book().tags();
    let rounds: usize = if use_max_items {
        // No fixed count: the loop below stops on the first round that cannot be
        // completed, which is what "as much as possible" means and is bounded by
        // the per-cell stack cap regardless.
        usize::MAX
    } else {
        1
    };
    let mut placed_any = false;
    for _ in 0..rounds {
        // A round is all-or-nothing: a partially-filled extra round would leave the
        // grid holding a shape that matches no recipe.
        let mut taken: Vec<(usize, ItemStack)> = Vec::new();
        let mut ok = true;
        for (cell, ingredient) in placement.iter().enumerate() {
            let Some(ingredient) = ingredient else { continue };
            // The item's *own* cap, not a constant 64 — vanilla's
            // `clampToMaxStackSize`. A grid cell holding 16 ender pearls is full.
            let held = grid.input(cell).map_or(0, |s| s.count);
            let cap = grid
                .input(cell)
                .map_or(64, crate::container_click::max_stack_size);
            if held >= cap {
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
        // Clearing is deferred until the first round that is going to succeed;
        // a round that fails leaves the existing grid untouched.
        if !placed_any && !accumulating {
            for cell in 0..cells {
                if let Some(existing) = grid.input(cell).cloned() {
                    grid.set_input(cell, None);
                    inventory.add(existing);
                }
            }
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

/// The result a `width × height` grid of `cells` produces, from [`recipe_book`].
///
/// The free function behind [`CraftingState::recompute`], extracted so the click
/// machine can re-derive the result **inside** one click
/// ([`crate::container_click::do_click_with`]) rather than only after it.
/// Recomputing after each grid change makes a shift-click on the result craft
/// repeatedly: the loop examines the refilled result until the grid or inventory
/// cannot supply another craft.
///
/// `cells` is row-major and must be `width * height` long; a shorter one is padded
/// with empties rather than rejected, because the caller is a slot vector and a
/// length mismatch there is a layout bug that should not silently mint a result.
#[must_use]
pub fn derive_result(width: usize, height: usize, cells: &[Option<ItemStack>]) -> Option<ItemStack> {
    if width == 0 || height == 0 {
        return None;
    }
    let items = (0..width * height)
        .map(|i| cells.get(i).and_then(|slot| slot.as_ref()).map(|stack| stack.item.clone()))
        .collect();
    let grid = CraftingGrid::new(width, height, items);
    if grid.is_empty() {
        return None;
    }
    recipe_book().match_grid(&grid).map(|result| {
        // `lodestone_game::item::ItemStack` and `lodestone_model::ItemStack` are two
        // distinct types (a signed working count vs. an unsigned stored one); the
        // recipe corpus speaks the former and every slot in this crate the latter.
        ItemStack::new(result.item().clone(), result.count().max(0).unsigned_abs())
    })
}

/// The server's own crafting grid and the result *it* computed.
///
/// One per open crafting menu. The player inventory screen's 2×2 and a crafting
/// table's 3×3 are the same type at different dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CraftingState {
    width: usize,
    height: usize,
    inputs: Vec<Option<ItemStack>>,
    result: Option<ItemStack>,
}

impl CraftingState {
    /// A 2×2 grid for the player inventory screen.
    #[must_use]
    pub fn player() -> Self {
        Self::new(2, 2)
    }

    /// A 3×3 grid for a crafting table.
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

    /// Empty every input cell and the result when closing a
    /// crafting menu (the grid's contents are returned to the player, and a
    /// closed menu keeps nothing).
    pub fn clear(&mut self) {
        for cell in &mut self.inputs {
            *cell = None;
        }
        self.result = None;
    }

    fn recompute(&mut self) {
        self.result = derive_result(self.width, self.height, &self.inputs);
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

    /// A shaped recipe with the expected result read from the bundled datapack
    /// rather than from the matcher: `crafting_table.json` is four
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

    /// **The 8-planks case, as the owner described it.** Place `crafting_table`
    /// twice from a stack of 8 oak planks and the grid must hold *two* crafts'
    /// worth while the source stack has dropped by exactly eight.
    ///
    /// 8 is the right fixture size precisely because it makes 1 / 8 / 64
    /// distinguishable: a one-craft-per-click implementation leaves 4 in the grid
    /// and 4 in the slot, a "clears and re-places" one leaves 4 and 4 *as well*,
    /// and only the accumulate semantics leave 2-per-cell and 0. Reading the source
    /// slot alone would not separate the first two.
    ///
    /// The expected numbers come from `crafting_table.json` (four
    /// `#minecraft:planks` in a 2×2, yielding one table) plus arithmetic, not from
    /// this module: two crafts is 8 planks, which is the whole stack.
    #[test]
    fn clicking_the_same_recipe_twice_places_two_crafts_worth() {
        let (_, recipe) = recipe_book()
            .iter()
            .find(|(id, _)| id.to_string() == "minecraft:crafting_table")
            .expect("the bundled corpus carries crafting_table");
        let mut inventory = crate::inventory::PlayerInventory::new();
        inventory.set_native(9, Some(stack("minecraft:oak_planks", 8)));
        let mut grid = CraftingState::player();

        assert!(place_recipe(&mut inventory, &mut grid, recipe, false));
        for cell in 0..4 {
            assert_eq!(
                grid.input(cell).map(|s| s.count),
                Some(1),
                "one plain click places exactly one craft's worth per cell"
            );
        }
        assert_eq!(
            inventory.native(9).map(|s| s.count),
            Some(4),
            "four planks left the source slot, not the whole stack"
        );

        // A second click on the same recipe adds another craft to the existing grid.
        assert!(place_recipe(&mut inventory, &mut grid, recipe, false));
        for cell in 0..4 {
            assert_eq!(
                grid.input(cell).map(|s| s.count),
                Some(2),
                "clicking the same recipe again must ADD another craft's worth — 1 here \
                 means the grid was cleared and re-placed, which is the reported bug"
            );
        }
        assert_eq!(
            inventory.native(9),
            None,
            "and the source stack is now spent exactly: 2 crafts x 4 planks = 8"
        );
        assert_eq!(
            grid.result().map(|r| r.item.to_string()),
            Some("minecraft:crafting_table".to_string()),
            "the derived result must survive the top-up, or the grid no longer matches"
        );

        // A third click cannot be afforded, and must therefore change **nothing** —
        // not empty the grid into the inventory, which is what a clear-first
        // implementation does and is a visible change on a click that did nothing.
        assert!(!place_recipe(&mut inventory, &mut grid, recipe, false));
        for cell in 0..4 {
            assert_eq!(grid.input(cell).map(|s| s.count), Some(2));
        }
    }

    /// A **different** recipe clears the existing grid back into the inventory
    /// before placing its own ingredients.
    ///
    /// This is the control for the test above: if `grid_matches` answered `true`
    /// unconditionally, that test would pass and this one would find planks still in
    /// the grid alongside sticks.
    #[test]
    fn a_different_recipe_clears_the_grid_back_to_the_inventory() {
        let book = recipe_book();
        let (_, table) = book
            .iter()
            .find(|(id, _)| id.to_string() == "minecraft:crafting_table")
            .expect("crafting_table");
        let (_, plate) = book
            .iter()
            .find(|(id, _)| id.to_string() == "minecraft:oak_pressure_plate")
            .expect("oak_pressure_plate");

        let mut inventory = crate::inventory::PlayerInventory::new();
        inventory.set_native(9, Some(stack("minecraft:oak_planks", 8)));
        let mut grid = CraftingState::player();
        assert!(place_recipe(&mut inventory, &mut grid, table, false));

        // `oak_pressure_plate` is two planks side by side, so cells 2 and 3 must
        // end up empty — they are occupied before this call.
        assert!(place_recipe(&mut inventory, &mut grid, plate, false));
        assert_eq!(grid.input(0).map(|s| s.count), Some(1));
        assert_eq!(grid.input(1).map(|s| s.count), Some(1));
        assert_eq!(
            (grid.input(2), grid.input(3)),
            (None, None),
            "a cell the new recipe does not use must be cleared, not left holding the old \
             recipe's ingredient"
        );
        assert_eq!(
            grid.result().map(|r| r.item.to_string()),
            Some("minecraft:oak_pressure_plate".to_string())
        );
        // 8 planks total, 2 in the grid, so 6 back in the inventory. Nothing may be
        // lost on the round trip through `PlayerInventory::add`.
        let in_inventory: u32 = (0..36)
            .filter_map(|i| inventory.native(i))
            .filter(|s| s.item.to_string() == "minecraft:oak_planks")
            .map(|s| s.count)
            .sum();
        assert_eq!(
            in_inventory, 6,
            "the cleared grid's planks must come back — 8 total minus the 2 now in the grid"
        );
    }

    /// Shift-click draws **across multiple stacks**, which is the third clause of
    /// the owner's spec, and stops at the per-cell cap rather than at one round.
    ///
    /// Two stacks of 40 in different slots: a `use_max_items` fill of a 2×2 recipe
    /// wants 4 per round, so 80 planks affords 20 crafts and the loop must cross the
    /// slot boundary to get there. An implementation that only ever read one source
    /// slot would stop at 10.
    #[test]
    fn a_shift_click_draws_across_multiple_source_stacks() {
        let (_, recipe) = recipe_book()
            .iter()
            .find(|(id, _)| id.to_string() == "minecraft:crafting_table")
            .expect("crafting_table");
        let mut inventory = crate::inventory::PlayerInventory::new();
        inventory.set_native(9, Some(stack("minecraft:oak_planks", 40)));
        inventory.set_native(10, Some(stack("minecraft:oak_planks", 40)));
        let mut grid = CraftingState::player();

        assert!(place_recipe(&mut inventory, &mut grid, recipe, true));
        for cell in 0..4 {
            assert_eq!(
                grid.input(cell).map(|s| s.count),
                Some(20),
                "80 planks over a 4-cell recipe is 20 crafts; 10 means only one source \
                 stack was consulted"
            );
        }
        assert!(
            (0..36).all(|i| inventory.native(i).is_none()),
            "both source stacks must be spent"
        );
    }
}
