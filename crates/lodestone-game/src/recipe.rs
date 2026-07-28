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
#[derive(Debug, Default, Clone)]
pub struct RecipeBook {
    /// Sorted by id so matching is deterministic.
    recipes: Vec<(Identifier, Recipe)>,
    tags: TagResolver,
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
        }
    }

    /// The tag resolver ingredients are matched against.
    #[must_use]
    pub fn tags(&self) -> &TagResolver {
        &self.tags
    }

    /// Adds or replaces a recipe, keeping the corpus id-sorted.
    pub fn insert(&mut self, id: Identifier, recipe: Recipe) {
        match self.recipes.binary_search_by(|(k, _)| k.cmp(&id)) {
            Ok(at) => self.recipes[at] = (id, recipe),
            Err(at) => self.recipes.insert(at, (id, recipe)),
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
    #[must_use]
    pub fn match_grid_entry(&self, grid: &CraftingGrid) -> Option<(&Identifier, &ItemStack)> {
        self.recipes
            .iter()
            .find_map(|(id, r)| r.match_grid(grid, &self.tags).map(|res| (id, res)))
    }

    /// The result of the first grid recipe matching `grid`, or `None` if the
    /// grid crafts nothing.
    #[must_use]
    pub fn match_grid(&self, grid: &CraftingGrid) -> Option<&ItemStack> {
        self.match_grid_entry(grid).map(|(_, result)| result)
    }
}
