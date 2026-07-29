//! Recipes: the version-free crafting model.
//!
//! Minecraft's recipe corpus is data-driven. Vanilla ships every recipe as a
//! JSON file (`data/minecraft/recipe/*.json`) plus the item **tags** that
//! ingredients may reference (`data/minecraft/tags/item/*.json`). This module
//! defines the canonical in-memory shapes and the matching rules, and — behind
//! the `json` feature — a loader from Mojang's own generated JSON.
//!
//! ## Matching rules that bite
//!
//! * **Shaped** recipes carry a `w×h` pattern that must be found *anywhere*
//!   inside the crafting grid (a 2×2 pattern matches in a 3×3 grid), and by
//!   default it also matches **mirrored** left-to-right. Cells the pattern does
//!   not cover must be empty.
//! * **Shapeless** recipes match as a **multiset**: the grid must contain
//!   exactly one item per ingredient, assignable one-to-one. This is a bipartite
//!   matching, not a naive "each ingredient appears somewhere" scan — the naive
//!   version accepts a grid that reuses one item for two ingredients.
//! * **Ingredients** are item ids, `#tag` references, or an array of options
//!   (each an item or tag). Tags can nest, so resolution is recursive.
//!
//! Everything here is version-free: ingredients and results are
//! [`Identifier`]s, never numeric ids. A version adapter is responsible for
//! lowering older wire formats into these shapes.

use std::collections::{HashMap, HashSet};

use lodestone_model::Identifier;

use crate::item::ItemStack;

/// A single ingredient slot: an item, a tag, or a choice among options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ingredient {
    /// Exactly one item.
    Item(Identifier),
    /// Any item in the referenced item tag.
    Tag(Identifier),
    /// Any of the listed options (each itself an item or tag).
    Any(Vec<Ingredient>),
}

impl Ingredient {
    /// Returns whether `item` satisfies this ingredient, resolving tags through
    /// `tags`.
    #[must_use]
    pub fn matches(&self, item: &Identifier, tags: &TagResolver) -> bool {
        match self {
            Ingredient::Item(id) => id == item,
            Ingredient::Tag(tag) => tags.contains(tag, item),
            Ingredient::Any(opts) => opts.iter().any(|o| o.matches(item, tags)),
        }
    }
}

/// Resolves item tags (possibly nested) to their transitive item sets.
///
/// A tag's `values` list may contain item ids or `#tag` references; references
/// are followed recursively with cycle protection. The resolver memoises each
/// tag's flattened set on first query.
#[derive(Debug, Default, Clone)]
pub struct TagResolver {
    /// Raw tag definitions: tag id -> its direct entries.
    raw: HashMap<Identifier, Vec<TagEntry>>,
    /// Memoised transitive resolutions.
    cache: std::cell::RefCell<HashMap<Identifier, HashSet<Identifier>>>,
}

/// One entry in a tag's `values` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagEntry {
    /// A concrete item.
    Item(Identifier),
    /// A reference to another tag (written `#namespace:path`).
    Tag(Identifier),
}

impl TagResolver {
    /// Creates an empty resolver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a tag definition.
    pub fn insert(&mut self, tag: Identifier, entries: Vec<TagEntry>) {
        self.cache.borrow_mut().clear();
        self.raw.insert(tag, entries);
    }

    /// Number of registered tags.
    #[must_use]
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// Whether no tags are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Returns whether `item` is a member of `tag` (transitively).
    #[must_use]
    pub fn contains(&self, tag: &Identifier, item: &Identifier) -> bool {
        self.resolve(tag).contains(item)
    }

    /// Returns the fully-flattened item set for `tag`. Unknown tags resolve to
    /// the empty set.
    #[must_use]
    pub fn resolve(&self, tag: &Identifier) -> HashSet<Identifier> {
        if let Some(hit) = self.cache.borrow().get(tag) {
            return hit.clone();
        }
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        self.resolve_into(tag, &mut out, &mut seen);
        self.cache.borrow_mut().insert(tag.clone(), out.clone());
        out
    }

    fn resolve_into(
        &self,
        tag: &Identifier,
        out: &mut HashSet<Identifier>,
        seen: &mut HashSet<Identifier>,
    ) {
        if !seen.insert(tag.clone()) {
            return; // cycle guard
        }
        let Some(entries) = self.raw.get(tag) else {
            return;
        };
        for entry in entries {
            match entry {
                TagEntry::Item(id) => {
                    out.insert(id.clone());
                }
                TagEntry::Tag(nested) => self.resolve_into(nested, out, seen),
            }
        }
    }
}

/// The contents of a crafting grid being matched against a recipe.
#[derive(Debug, Clone)]
pub struct CraftingGrid {
    width: usize,
    height: usize,
    /// Row-major, `width * height` cells; `None` is empty.
    cells: Vec<Option<Identifier>>,
}

impl CraftingGrid {
    /// Builds a grid from row-major cells. Panics if `cells.len() != w*h`.
    #[must_use]
    pub fn new(width: usize, height: usize, cells: Vec<Option<Identifier>>) -> Self {
        assert_eq!(cells.len(), width * height, "grid cell count mismatch");
        Self {
            width,
            height,
            cells,
        }
    }

    /// Grid width in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Grid height in cells.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// The item in cell `(x, y)`, or `None` if it is empty or out of bounds.
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> Option<&Identifier> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells[y * self.width + x].as_ref()
    }

    /// Whether every cell is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.iter().all(Option::is_none)
    }

    /// The non-empty items in the grid, order unspecified.
    fn occupied(&self) -> Vec<&Identifier> {
        self.cells.iter().flatten().collect()
    }
}

/// A recipe in canonical form.
///
/// Types that are not grid-craftable (smelting, stonecutting, smithing, special
/// crafters) are still represented so the whole corpus can be loaded and
/// counted, but only [`Shaped`](Recipe::Shaped) and
/// [`Shapeless`](Recipe::Shapeless) implement [`matches`](Recipe::matches)
/// against a [`CraftingGrid`].
#[derive(Debug, Clone, PartialEq)]
pub enum Recipe {
    /// A shaped crafting recipe.
    Shaped(ShapedRecipe),
    /// A shapeless crafting recipe.
    Shapeless(ShapelessRecipe),
    /// A furnace/blast/smoker/campfire cooking recipe.
    Cooking(CookingRecipe),
    /// A stonecutter recipe.
    Stonecutting {
        /// The single input.
        ingredient: Ingredient,
        /// The output.
        result: ItemStack,
    },
    /// A smithing-table transform (netherite upgrade etc.).
    SmithingTransform {
        /// Upgrade template ingredient.
        template: Ingredient,
        /// Base item ingredient.
        base: Ingredient,
        /// Addition (material) ingredient.
        addition: Ingredient,
        /// The output.
        result: ItemStack,
    },
    /// A smithing-table armour trim.
    SmithingTrim {
        /// Trim template ingredient.
        template: Ingredient,
        /// Base (trimmable) ingredient.
        base: Ingredient,
        /// Addition (trim material) ingredient.
        addition: Ingredient,
    },
    /// A transmute recipe (input + material -> result, copying components).
    Transmute {
        /// The item being transmuted.
        input: Ingredient,
        /// The material consumed.
        material: Ingredient,
        /// The output item id.
        result: ItemStack,
    },
    /// A hard-coded special recipe with no data-driven ingredients (firework
    /// crafting, map cloning, etc.). The string is its recipe type path.
    Special(String),
}

/// The kind of cooking recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookingKind {
    /// Furnace smelting.
    Smelting,
    /// Blast furnace.
    Blasting,
    /// Smoker.
    Smoking,
    /// Campfire.
    CampfireCooking,
}

/// A cooking recipe.
#[derive(Debug, Clone, PartialEq)]
pub struct CookingRecipe {
    /// Which appliance.
    pub kind: CookingKind,
    /// The single input.
    pub ingredient: Ingredient,
    /// The output.
    pub result: ItemStack,
    /// Experience granted.
    pub experience: f32,
    /// Cooking time in ticks (default 200 for smelting).
    pub cooking_time: i32,
}

/// A shaped recipe: a `width × height` pattern with optional cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedRecipe {
    width: usize,
    height: usize,
    /// Row-major pattern cells; `None` means "must be empty".
    pattern: Vec<Option<Ingredient>>,
    result: ItemStack,
    mirror: bool,
    group: Option<String>,
}

impl ShapedRecipe {
    /// Constructs a shaped recipe from a row-major pattern.
    #[must_use]
    pub fn new(
        width: usize,
        height: usize,
        pattern: Vec<Option<Ingredient>>,
        result: ItemStack,
    ) -> Self {
        assert_eq!(pattern.len(), width * height, "pattern cell count mismatch");
        Self {
            width,
            height,
            pattern,
            result,
            mirror: true,
            group: None,
        }
    }

    /// Disables mirrored matching (vanilla allows disabling it per recipe).
    #[must_use]
    pub fn without_mirror(mut self) -> Self {
        self.mirror = false;
        self
    }

    /// The recipe result.
    #[must_use]
    pub fn result(&self) -> &ItemStack {
        &self.result
    }

    fn pattern_at(&self, x: usize, y: usize, mirrored: bool) -> Option<&Ingredient> {
        let col = if mirrored { self.width - 1 - x } else { x };
        self.pattern[y * self.width + col].as_ref()
    }

    /// The number of non-empty cells in the pattern.
    ///
    /// Every successful match (at any offset or mirroring) requires the
    /// grid's occupied-cell count to equal this exactly: an in-pattern `None`
    /// cell only matches an empty grid cell, and every cell outside the
    /// pattern's footprint must also be empty (see [`matches_at`]). This is
    /// what makes occupied-cell count a sound, cheap pre-filter for recipe
    /// lookup — see [`RecipeBook`]'s index.
    ///
    /// [`matches_at`]: Self::matches_at
    fn filled_cell_count(&self) -> usize {
        self.pattern.iter().flatten().count()
    }

    /// Whether this recipe matches `grid`, trying every offset and (if enabled)
    /// the mirrored pattern.
    #[must_use]
    pub fn matches(&self, grid: &CraftingGrid, tags: &TagResolver) -> bool {
        if self.width > grid.width || self.height > grid.height {
            return false;
        }
        let mirrors: &[bool] = if self.mirror {
            &[false, true]
        } else {
            &[false]
        };
        for &mirrored in mirrors {
            for oy in 0..=(grid.height - self.height) {
                for ox in 0..=(grid.width - self.width) {
                    if self.matches_at(grid, ox, oy, mirrored, tags) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn matches_at(
        &self,
        grid: &CraftingGrid,
        ox: usize,
        oy: usize,
        mirrored: bool,
        tags: &TagResolver,
    ) -> bool {
        for gy in 0..grid.height {
            for gx in 0..grid.width {
                let in_pattern =
                    gx >= ox && gx < ox + self.width && gy >= oy && gy < oy + self.height;
                let cell = grid.get(gx, gy);
                if in_pattern {
                    let ing = self.pattern_at(gx - ox, gy - oy, mirrored);
                    match (ing, cell) {
                        (Some(ing), Some(item)) if ing.matches(item, tags) => {}
                        (None, None) => {}
                        _ => return false,
                    }
                } else if cell.is_some() {
                    return false;
                }
            }
        }
        true
    }
}

/// A shapeless recipe: an unordered multiset of ingredients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapelessRecipe {
    ingredients: Vec<Ingredient>,
    result: ItemStack,
    group: Option<String>,
}

impl ShapelessRecipe {
    /// Constructs a shapeless recipe.
    #[must_use]
    pub fn new(ingredients: Vec<Ingredient>, result: ItemStack) -> Self {
        Self {
            ingredients,
            result,
            group: None,
        }
    }

    /// The recipe result.
    #[must_use]
    pub fn result(&self) -> &ItemStack {
        &self.result
    }

    /// Whether this recipe matches `grid`. The grid's occupied items must equal
    /// the ingredient count and admit a one-to-one assignment.
    #[must_use]
    pub fn matches(&self, grid: &CraftingGrid, tags: &TagResolver) -> bool {
        let items = grid.occupied();
        if items.len() != self.ingredients.len() {
            return false;
        }
        // Bipartite perfect matching: each ingredient to a distinct item.
        let mut used = vec![false; items.len()];
        assign(&self.ingredients, &items, &mut used, 0, tags)
    }
}

/// Backtracking assignment of ingredients to distinct grid items.
fn assign(
    ingredients: &[Ingredient],
    items: &[&Identifier],
    used: &mut [bool],
    idx: usize,
    tags: &TagResolver,
) -> bool {
    if idx == ingredients.len() {
        return true;
    }
    for (i, item) in items.iter().enumerate() {
        if !used[i] && ingredients[idx].matches(item, tags) {
            used[i] = true;
            if assign(ingredients, items, used, idx + 1, tags) {
                return true;
            }
            used[i] = false;
        }
    }
    false
}

impl Recipe {
    /// Attempts to match this recipe against a crafting grid. Non-crafting
    /// recipe types always return `None`; crafting recipes return their result
    /// on a match.
    #[must_use]
    pub fn match_grid(&self, grid: &CraftingGrid, tags: &TagResolver) -> Option<&ItemStack> {
        match self {
            Recipe::Shaped(r) if r.matches(grid, tags) => Some(&r.result),
            Recipe::Shapeless(r) if r.matches(grid, tags) => Some(&r.result),
            _ => None,
        }
    }

    /// Whether this recipe can ever match a [`CraftingGrid`] — i.e. whether it
    /// is one of the two grid-crafting kinds.
    #[must_use]
    pub fn is_grid_recipe(&self) -> bool {
        matches!(self, Recipe::Shaped(_) | Recipe::Shapeless(_))
    }

    /// The exact number of occupied grid cells required for this recipe to
    /// have any chance of matching, or `None` for a non-grid recipe kind
    /// (which never matches [`match_grid`](Self::match_grid) regardless).
    ///
    /// This is a necessary condition, not sufficient — a grid with the right
    /// occupied count can still fail on item identity — but it is *exact*:
    /// no grid with a different occupied count can ever match. See
    /// [`ShapedRecipe::filled_cell_count`] for the shaped proof; shapeless is
    /// immediate from [`ShapelessRecipe::matches`]'s length check.
    fn occupied_cell_count(&self) -> Option<usize> {
        match self {
            Recipe::Shaped(r) => Some(r.filled_cell_count()),
            Recipe::Shapeless(r) => Some(r.ingredients.len()),
            _ => None,
        }
    }
}

/// A loaded recipe corpus: every recipe, keyed by its data id, plus the item
/// tags its ingredients reference.
///
/// This is the aggregate that turns [`Recipe`] from a lone data type into
/// something a client can query. Build it with [`RecipeBook::insert`], or —
/// behind the `json` feature — load a whole vanilla datapack tree with
/// [`crate::recipe_json::load_data_root`].
///
/// ## Ordering
///
/// [`match_grid`](Self::match_grid) returns the **first** matching recipe in
/// id order. Vanilla's `RecipeManager` iterates an unordered map, so it relies
/// on the corpus containing no two grid recipes that match the same grid; the
/// sorted order here just makes our answer deterministic when a datapack
/// violates that.
///
/// ## What this is *not*
///
/// It is not the source of truth for an open crafting menu's result slot. A
/// vanilla server computes that itself (`CraftingMenu.slotsChanged` sends a
/// `container_set_slot` for slot 0), and this client honours the server the
/// same way. Use this book for the recipe-book UI, ghost recipes, and
/// latency-hiding prediction — never to overwrite a server-sent result slot.
///
/// ## Lookup is indexed, not scanned
///
/// [`match_grid_entry`](Self::match_grid_entry) does not walk all ~1585
/// recipes per query. [`Recipe::occupied_cell_count`] proves that a grid can
/// only ever match a shaped or shapeless recipe whose required occupied-cell
/// count equals the grid's actual occupied-cell count — a shaped pattern's
/// `None` cells only match empty grid cells and every cell outside the
/// pattern's footprint must be empty too, and shapeless matching is a
/// straight length check. `grid_index` buckets grid-matchable recipe ids by
/// that count, so a query first narrows to the (typically tiny) bucket for
/// its own occupied count, then runs the real `match_grid` only against that
/// bucket. Occupied-cell count was chosen over an ingredient-based signature
/// because it needs no tag resolution to compute (tags can only be resolved
/// against a concrete item, and the grid is the only concrete thing at query
/// time) and it is *exact*, not a heuristic — it can never discard a real
/// match. Non-grid recipe kinds (cooking, stonecutting, smithing, transmute,
/// special) are never indexed; they always return `None` from `match_grid`
/// regardless of any grid, so omitting them from the index is a no-op, not a
/// behaviour change.
///
/// Each bucket is kept sorted by id, exactly mirroring `recipes`' own order,
/// so scanning a bucket and scanning the whole corpus visit surviving
/// candidates in the same relative order — which is what preserves the
/// documented "first match in id order" precedence exactly. See the
/// `indexed_lookup_matches_brute_force_scan` test for the equivalence check.
#[derive(Debug, Default, Clone)]
pub struct RecipeBook {
    /// Sorted by id so matching is deterministic.
    recipes: Vec<(Identifier, Recipe)>,
    tags: TagResolver,
    /// Grid-matchable recipe ids bucketed by [`Recipe::occupied_cell_count`].
    /// Each bucket is sorted by id (see the type-level docs above).
    grid_index: HashMap<usize, Vec<Identifier>>,
}

impl RecipeBook {
    /// An empty book with no tags.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty book that resolves ingredients against `tags`.
    #[must_use]
    pub fn with_tags(tags: TagResolver) -> Self {
        Self {
            recipes: Vec::new(),
            tags,
            grid_index: HashMap::new(),
        }
    }

    /// The tag resolver ingredients are matched against.
    #[must_use]
    pub fn tags(&self) -> &TagResolver {
        &self.tags
    }

    /// Adds or replaces a recipe, keeping the corpus id-sorted and the grid
    /// index (see the type docs) in sync.
    pub fn insert(&mut self, id: Identifier, recipe: Recipe) {
        let new_bucket = recipe.occupied_cell_count();
        match self.recipes.binary_search_by(|(k, _)| k.cmp(&id)) {
            Ok(at) => {
                // Replacing an existing id: its recipe kind/shape may have
                // changed, so its old bucket membership (if any) may no
                // longer be correct. Drop it before inserting the new one.
                if let Some(old_bucket) = self.recipes[at].1.occupied_cell_count() {
                    remove_sorted(self.grid_index.entry(old_bucket).or_default(), &id);
                }
                self.recipes[at] = (id.clone(), recipe);
            }
            Err(at) => self.recipes.insert(at, (id.clone(), recipe)),
        }
        if let Some(bucket) = new_bucket {
            insert_sorted(self.grid_index.entry(bucket).or_default(), id);
        }
    }

    /// Number of recipes loaded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    /// Whether the book holds no recipes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    /// Iterates every `(id, recipe)` pair in id order.
    pub fn iter(&self) -> impl Iterator<Item = (&Identifier, &Recipe)> {
        self.recipes.iter().map(|(k, v)| (k, v))
    }

    /// The first grid recipe matching `grid`, with its id and result.
    ///
    /// See the type-level "Lookup is indexed, not scanned" docs: this narrows
    /// to the bucket of recipes whose required occupied-cell count equals
    /// `grid`'s, then runs the real match only against that bucket, in the
    /// same id order the unindexed scan would have used.
    #[must_use]
    pub fn match_grid_entry(&self, grid: &CraftingGrid) -> Option<(&Identifier, &ItemStack)> {
        let occupied = grid.occupied().len();
        let bucket = self.grid_index.get(&occupied)?;
        for id in bucket {
            // The index only ever holds ids that are still in `recipes` (see
            // `insert`), so this lookup succeeding is an invariant, not a
            // possibility; `continue` rather than panicking keeps a violated
            // invariant a missed match instead of a crash.
            let Ok(at) = self.recipes.binary_search_by(|(k, _)| k.cmp(id)) else {
                continue;
            };
            let (id, recipe) = &self.recipes[at];
            if let Some(result) = recipe.match_grid(grid, &self.tags) {
                return Some((id, result));
            }
        }
        None
    }

    /// The result of the first grid recipe matching `grid`, or `None` if the
    /// grid crafts nothing.
    #[must_use]
    pub fn match_grid(&self, grid: &CraftingGrid) -> Option<&ItemStack> {
        self.match_grid_entry(grid).map(|(_, result)| result)
    }
}

/// Inserts `id` into a bucket kept sorted in the same order as
/// [`RecipeBook::recipes`], via the same binary-search-then-insert shape as
/// [`RecipeBook::insert`] uses for the corpus itself.
fn insert_sorted(bucket: &mut Vec<Identifier>, id: Identifier) {
    if let Err(at) = bucket.binary_search(&id) {
        bucket.insert(at, id);
    }
}

/// Removes `id` from a sorted bucket, if present.
fn remove_sorted(bucket: &mut Vec<Identifier>, id: &Identifier) {
    if let Ok(at) = bucket.binary_search(id) {
        bucket.remove(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Identifier {
        s.parse().expect("valid id")
    }

    fn stack(name: &str, count: i32) -> ItemStack {
        ItemStack::new(id(name), count)
    }

    fn grid(width: usize, height: usize, items: &[Option<&str>]) -> CraftingGrid {
        CraftingGrid::new(width, height, items.iter().map(|i| i.map(id)).collect())
    }

    /// The pre-index behaviour, verbatim: a linear scan in id order returning
    /// the first match. This is the oracle the index must never disagree
    /// with.
    fn brute_force_match_grid_entry<'a>(
        book: &'a RecipeBook,
        grid: &CraftingGrid,
    ) -> Option<(&'a Identifier, &'a ItemStack)> {
        book.recipes
            .iter()
            .find_map(|(id, r)| r.match_grid(grid, &book.tags).map(|res| (id, res)))
    }

    /// A small but representative corpus: a shaped recipe, a same-occupied-
    /// count shaped recipe with different ingredients (exercises a bucket
    /// with more than one candidate), a bordered 3x3 shaped recipe (a
    /// different occupied count), a shapeless recipe using a tag ingredient,
    /// a pair of shaped recipes that deliberately collide on the same grid
    /// (to pin down id-order precedence), and a non-grid recipe kind (to
    /// confirm it is correctly excluded from the index without changing the
    /// answer).
    fn book() -> RecipeBook {
        let mut tags = TagResolver::new();
        tags.insert(
            id("minecraft:planks"),
            vec![TagEntry::Item(id("minecraft:oak_planks"))],
        );
        let mut book = RecipeBook::with_tags(tags);

        // Shaped, occupied = 4.
        book.insert(
            id("minecraft:test_shaped_full"),
            Recipe::Shaped(ShapedRecipe::new(
                2,
                2,
                vec![
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                ],
                stack("minecraft:crafting_table", 1),
            )),
        );
        // Shaped, occupied = 4, same bucket, disjoint ingredient so it can
        // never match the same grid as the recipe above.
        book.insert(
            id("minecraft:test_shaped_collide_bucket"),
            Recipe::Shaped(ShapedRecipe::new(
                2,
                2,
                vec![
                    Some(Ingredient::Item(id("minecraft:cobblestone"))),
                    Some(Ingredient::Item(id("minecraft:cobblestone"))),
                    Some(Ingredient::Item(id("minecraft:cobblestone"))),
                    Some(Ingredient::Item(id("minecraft:cobblestone"))),
                ],
                stack("minecraft:test_result_b", 1),
            )),
        );
        // Shaped with holes, occupied = 8 (a different bucket entirely).
        book.insert(
            id("minecraft:test_shaped_border"),
            Recipe::Shaped(ShapedRecipe::new(
                3,
                3,
                vec![
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    None,
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                    Some(Ingredient::Item(id("minecraft:oak_planks"))),
                ],
                stack("minecraft:chest", 1),
            )),
        );
        // Shapeless with a tag ingredient, occupied = 3.
        book.insert(
            id("minecraft:test_shapeless"),
            Recipe::Shapeless(ShapelessRecipe::new(
                vec![
                    Ingredient::Item(id("minecraft:coal")),
                    Ingredient::Item(id("minecraft:stick")),
                    Ingredient::Tag(id("minecraft:planks")),
                ],
                stack("minecraft:test_torch", 4),
            )),
        );
        // Two 1x1 shaped recipes that both match a lone stick — a corpus
        // that violates the "no two grid recipes match the same grid"
        // assumption on purpose, to pin the id-order tiebreak.
        book.insert(
            id("minecraft:aaa_conflict"),
            Recipe::Shaped(ShapedRecipe::new(
                1,
                1,
                vec![Some(Ingredient::Item(id("minecraft:stick")))],
                stack("minecraft:conflict_winner", 1),
            )),
        );
        book.insert(
            id("minecraft:zzz_conflict"),
            Recipe::Shaped(ShapedRecipe::new(
                1,
                1,
                vec![Some(Ingredient::Item(id("minecraft:stick")))],
                stack("minecraft:conflict_loser", 1),
            )),
        );
        // A non-grid recipe kind, id-sorted in the middle of the corpus, to
        // confirm it is silently and correctly never returned.
        book.insert(
            id("minecraft:mmm_smelting"),
            Recipe::Cooking(CookingRecipe {
                kind: CookingKind::Smelting,
                ingredient: Ingredient::Item(id("minecraft:iron_ore")),
                result: stack("minecraft:iron_ingot", 1),
                experience: 0.7,
                cooking_time: 200,
            }),
        );

        book
    }

    /// Every grid below is checked against both the index and the brute-force
    /// scan; disagreement is the only real evidence the index changed
    /// behaviour. Includes a bucket with multiple candidates, a populated
    /// bucket where nothing matches, a missing bucket (no recipe has that
    /// occupied count), and a genuine two-recipe collision.
    #[test]
    fn indexed_lookup_matches_brute_force_scan() {
        let book = book();
        let cases: Vec<CraftingGrid> = vec![
            // Matches test_shaped_full.
            grid(
                2,
                2,
                &[
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                ],
            ),
            // Matches test_shaped_collide_bucket (same bucket, different item).
            grid(
                2,
                2,
                &[
                    Some("minecraft:cobblestone"),
                    Some("minecraft:cobblestone"),
                    Some("minecraft:cobblestone"),
                    Some("minecraft:cobblestone"),
                ],
            ),
            // Same occupied count (4, a populated bucket) but matches neither.
            grid(
                2,
                2,
                &[
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:cobblestone"),
                ],
            ),
            // Matches the bordered 3x3 (occupied = 8, a different bucket).
            grid(
                3,
                3,
                &[
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    None,
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                    Some("minecraft:oak_planks"),
                ],
            ),
            // Fully filled 3x3 (occupied = 9): NEGATIVE CASE, no recipe has
            // this bucket at all.
            grid(3, 3, &[Some("minecraft:oak_planks"); 9]),
            // Matches the shapeless recipe via the tag ingredient, in a
            // deliberately non-declaration order (shapeless is a multiset).
            grid(
                3,
                1,
                &[
                    Some("minecraft:oak_planks"),
                    Some("minecraft:coal"),
                    Some("minecraft:stick"),
                ],
            ),
            // Same occupied count (3, a populated bucket) but the third item
            // is not in the `minecraft:planks` tag: NEGATIVE CASE.
            grid(
                3,
                1,
                &[
                    Some("minecraft:birch_log"),
                    Some("minecraft:coal"),
                    Some("minecraft:stick"),
                ],
            ),
            // The genuine two-recipe collision.
            grid(1, 1, &[Some("minecraft:stick")]),
            // Fully empty grid: NEGATIVE CASE, no bucket for occupied = 0.
            grid(1, 1, &[None]),
        ];

        for case in &cases {
            assert_eq!(
                book.match_grid_entry(case),
                brute_force_match_grid_entry(&book, case),
                "indexed and brute-force lookups disagreed for grid {case:?}"
            );
        }
    }

    /// Pins the id-order precedence explicitly, not just via equivalence with
    /// the brute-force oracle: when two grid recipes both match, the one
    /// that sorts first by id wins, exactly as the unindexed scan's
    /// `find_map` over an id-sorted `Vec` always did.
    #[test]
    fn first_match_in_id_order_wins_on_a_genuine_collision() {
        let book = book();
        let g = grid(1, 1, &[Some("minecraft:stick")]);
        let (winner_id, winner_result) = book.match_grid_entry(&g).expect("both recipes match");
        assert_eq!(winner_id, &id("minecraft:aaa_conflict"));
        assert_eq!(winner_result, &stack("minecraft:conflict_winner", 1));
    }

    /// A grid whose occupied-cell count matches no recipe at all must miss
    /// via the empty/missing bucket path, not just happen to fall through a
    /// populated one.
    #[test]
    fn grid_with_no_matching_recipe_returns_none() {
        let book = book();
        let g = grid(3, 3, &[Some("minecraft:oak_planks"); 9]);
        assert_eq!(book.match_grid_entry(&g), None);
    }
}
