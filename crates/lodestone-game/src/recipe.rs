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

use lodestone_model::event::ClientEvent;
use lodestone_model::{Identifier, RecipeBookType, RecipeBookTypeSettings};

use crate::item::ItemStack;

/// Vanilla's `RecipeBookCategories` grouping
/// (`RecipeBookCategories.java:7-19`), captured from each recipe JSON's own
/// `"category"` field (present on 694 of 1585 recipes in 26.2's datapack;
/// [`recipe_json`](crate::recipe_json) parses it, see
/// [`RecipeBook::insert_with_category`]).
///
/// Vanilla actually registers a *separate* category object per recipe-book
/// type (`CRAFTING_MISC` and `FURNACE_MISC` are different registry entries),
/// but the underlying JSON string is the same handful of values regardless of
/// which book reads it, so one enum keyed by that string — paired with
/// [`RecipeBookType`] at the query site — covers every book without a
/// combinatorial variant list. A recipe with no `"category"` field defaults
/// to [`Misc`](Self::Misc), matching vanilla's own default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecipeCategory {
    /// `"building"` — crafting-table building blocks.
    Building,
    /// `"redstone"` — crafting-table redstone components.
    Redstone,
    /// `"equipment"` — crafting-table tools/armour/combat.
    Equipment,
    /// `"food"` — furnace/smoker cooking.
    Food,
    /// `"blocks"` — furnace/blast-furnace smelting.
    Blocks,
    /// No JSON category, or one this client does not recognise.
    Misc,
}

impl RecipeCategory {
    /// Parses a recipe JSON `"category"` value. Unknown strings (and the
    /// field's absence, handled by the caller) fall back to
    /// [`Misc`](Self::Misc) rather than failing the whole recipe's load — a
    /// tab miscategorised as "misc" is a cosmetic gap, not a reason to lose
    /// the recipe.
    #[must_use]
    pub fn from_json_str(s: &str) -> Self {
        match s {
            "building" => Self::Building,
            "redstone" => Self::Redstone,
            "equipment" => Self::Equipment,
            "food" => Self::Food,
            "blocks" => Self::Blocks,
            _ => Self::Misc,
        }
    }
}

/// The recipe-book tabs vanilla shows for `book_type`, in declaration order —
/// `RecipeBookCategories.java:7-19`, which is **not** alphabetical (the
/// rejected hypothesis: `[Blocks, Equipment, Misc, Redstone]`; the real order
/// interleaves `Redstone` before `Equipment`). A tab only actually appears if
/// at least one loaded recipe has that category (see
/// [`RecipeBook::visible_tabs`]) — this is the full, unfiltered set.
///
/// Notably `BlastFurnace` has no `Food` tab (`BLAST_FURNACE_BLOCKS`/
/// `_MISC` only, no `BLAST_FURNACE_FOOD` constant) and `Smoker` has only
/// `Food` (`SMOKER_FOOD` alone) — asymmetries a hand-derived "same three tabs
/// for every cooking appliance" guess would have missed.
#[must_use]
pub fn tabs_for(book_type: RecipeBookType) -> &'static [RecipeCategory] {
    use RecipeCategory::{Blocks, Building, Equipment, Food, Misc, Redstone};
    match book_type {
        RecipeBookType::Crafting => &[Building, Redstone, Equipment, Misc],
        RecipeBookType::Furnace => &[Food, Blocks, Misc],
        RecipeBookType::BlastFurnace => &[Blocks, Misc],
        RecipeBookType::Smoker => &[Food],
    }
}

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
    /// Recipe-book category (see [`RecipeCategory`]).
    pub category: RecipeCategory,
}

impl CookingRecipe {
    /// The furnace-family recipe book this recipe belongs to, or `None` for
    /// [`CookingKind::CampfireCooking`] — a campfire has no menu and
    /// therefore no recipe book at all.
    #[must_use]
    pub fn book_type(&self) -> Option<RecipeBookType> {
        match self.kind {
            CookingKind::Smelting => Some(RecipeBookType::Furnace),
            CookingKind::Blasting => Some(RecipeBookType::BlastFurnace),
            CookingKind::Smoking => Some(RecipeBookType::Smoker),
            CookingKind::CampfireCooking => None,
        }
    }

    /// A cooking recipe's "placement" is trivially its single ingredient in
    /// the furnace-family menu's one input slot (menu index `0` — see
    /// [`crate::menu::Menu::furnace`]), modelled as a `1×1` grid so it shares
    /// [`plan_auto_fill`] with the crafting-table case. Any `grid_w`/`grid_h`
    /// other than `(1, 1)` returns `None`.
    #[must_use]
    pub fn placement(&self, grid_w: usize, grid_h: usize) -> Option<Vec<Option<&Ingredient>>> {
        if (grid_w, grid_h) != (1, 1) {
            return None;
        }
        Some(vec![Some(&self.ingredient)])
    }
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
    category: RecipeCategory,
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
            category: RecipeCategory::Misc,
        }
    }

    /// Disables mirrored matching (vanilla allows disabling it per recipe).
    #[must_use]
    pub fn without_mirror(mut self) -> Self {
        self.mirror = false;
        self
    }

    /// Sets the recipe-book category (see [`RecipeCategory`]); recipes built
    /// with [`new`](Self::new) default to [`RecipeCategory::Misc`].
    #[must_use]
    pub fn with_category(mut self, category: RecipeCategory) -> Self {
        self.category = category;
        self
    }

    /// The concrete ingredient (or "must be empty") for each cell of a
    /// `grid_w × grid_h` crafting grid, placing the pattern at a fixed,
    /// canonical offset — top-left (`0, 0`), never mirrored.
    ///
    /// Vanilla's own recipe-book click (`ServerPlaceRecipe`/
    /// `PlaceRecipeHelper.calculatePlacementFor`) places at the position the
    /// server-sent `RecipeDisplay` itself carries. We do not decode that
    /// packet (`docs/crafting.md`'s "Remaining gaps"), so this is a
    /// deliberate, documented simplification rather than a guess at the real
    /// position: always the pattern's own unmirrored top-left. Returns `None`
    /// if the pattern does not fit the grid.
    #[must_use]
    pub fn placement(&self, grid_w: usize, grid_h: usize) -> Option<Vec<Option<&Ingredient>>> {
        if self.width > grid_w || self.height > grid_h {
            return None;
        }
        let mut out = vec![None; grid_w * grid_h];
        for y in 0..self.height {
            for x in 0..self.width {
                out[y * grid_w + x] = self.pattern_at(x, y, false);
            }
        }
        Some(out)
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
    category: RecipeCategory,
}

impl ShapelessRecipe {
    /// Constructs a shapeless recipe.
    #[must_use]
    pub fn new(ingredients: Vec<Ingredient>, result: ItemStack) -> Self {
        Self {
            ingredients,
            result,
            group: None,
            category: RecipeCategory::Misc,
        }
    }

    /// Sets the recipe-book category (see [`RecipeCategory`]); recipes built
    /// with [`new`](Self::new) default to [`RecipeCategory::Misc`].
    #[must_use]
    pub fn with_category(mut self, category: RecipeCategory) -> Self {
        self.category = category;
        self
    }

    /// The recipe result.
    #[must_use]
    pub fn result(&self) -> &ItemStack {
        &self.result
    }

    /// Ingredients in declaration order, one per grid cell in a
    /// `grid_w × grid_h` grid — vanilla has no notion of *position* for a
    /// shapeless recipe, so this simply lays them out left-to-right,
    /// top-to-bottom starting at cell `0`. Returns `None` if there are more
    /// ingredients than cells.
    #[must_use]
    pub fn placement(&self, grid_w: usize, grid_h: usize) -> Option<Vec<Option<&Ingredient>>> {
        if self.ingredients.len() > grid_w * grid_h {
            return None;
        }
        let mut out = vec![None; grid_w * grid_h];
        for (i, ing) in self.ingredients.iter().enumerate() {
            out[i] = Some(ing);
        }
        Some(out)
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

    /// The recipe-book category, for the three kinds the recipe-book UI
    /// shows recipes from. `None` for stonecutting/smithing/transmute/special
    /// — none of those have a browsable recipe book in this client (the
    /// stonecutter's own recipe list is a separate, unmodelled scroll list,
    /// see [`crate::menu::SpecialLayout::Stonecutter`]'s doc comment).
    #[must_use]
    pub fn category(&self) -> Option<RecipeCategory> {
        match self {
            Recipe::Shaped(r) => Some(r.category),
            Recipe::Shapeless(r) => Some(r.category),
            Recipe::Cooking(r) => Some(r.category),
            _ => None,
        }
    }

    /// Which recipe book (if any) browses this recipe — see
    /// [`RecipeBookType`].
    #[must_use]
    pub fn book_type(&self) -> Option<RecipeBookType> {
        match self {
            Recipe::Shaped(_) | Recipe::Shapeless(_) => Some(RecipeBookType::Crafting),
            Recipe::Cooking(r) => r.book_type(),
            _ => None,
        }
    }

    /// The item id this recipe produces, for the recipe-book panel's icon and
    /// search. `None` for kinds with no single fixed result id relevant here.
    #[must_use]
    pub fn result_item(&self) -> Option<&Identifier> {
        match self {
            Recipe::Shaped(r) => Some(r.result.item()),
            Recipe::Shapeless(r) => Some(r.result.item()),
            Recipe::Cooking(r) => Some(r.result.item()),
            Recipe::Stonecutting { result, .. } => Some(result.item()),
            _ => None,
        }
    }

    /// The full result stack (id **and** count), for the panel's item icon —
    /// see [`result_item`](Self::result_item) for the id-only accessor used by
    /// search.
    #[must_use]
    pub fn result_stack(&self) -> Option<&ItemStack> {
        match self {
            Recipe::Shaped(r) => Some(&r.result),
            Recipe::Shapeless(r) => Some(&r.result),
            Recipe::Cooking(r) => Some(&r.result),
            Recipe::Stonecutting { result, .. } => Some(result),
            _ => None,
        }
    }

    /// The concrete per-cell ingredient placement for a `grid_w × grid_h`
    /// grid — see [`ShapedRecipe::placement`], [`ShapelessRecipe::placement`]
    /// and [`CookingRecipe::placement`]. `None` for every non-placeable kind
    /// and for a grid the recipe cannot fit.
    #[must_use]
    pub fn placement(&self, grid_w: usize, grid_h: usize) -> Option<Vec<Option<&Ingredient>>> {
        match self {
            Recipe::Shaped(r) => r.placement(grid_w, grid_h),
            Recipe::Shapeless(r) => r.placement(grid_w, grid_h),
            Recipe::Cooking(r) => r.placement(grid_w, grid_h),
            _ => None,
        }
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

    /// Looks up a recipe by id.
    #[must_use]
    pub fn get(&self, id: &Identifier) -> Option<&Recipe> {
        let at = self.recipes.binary_search_by(|(k, _)| k.cmp(id)).ok()?;
        Some(&self.recipes[at].1)
    }

    // -- Recipe-book browsing (issue #163) -----------------------------

    /// As [`insert`](Self::insert), but also records an explicit
    /// [`RecipeCategory`] for the id, overriding whatever
    /// [`Recipe::category`] would otherwise report. Used by
    /// [`crate::recipe_json`] when a recipe JSON carries a `"category"`
    /// field — see that module's `parse_recipe`.
    pub fn insert_with_category(&mut self, id: Identifier, recipe: Recipe, category: RecipeCategory) {
        let recipe = match recipe {
            Recipe::Shaped(r) => Recipe::Shaped(r.with_category(category)),
            Recipe::Shapeless(r) => Recipe::Shapeless(r.with_category(category)),
            Recipe::Cooking(mut r) => {
                r.category = category;
                Recipe::Cooking(r)
            }
            other => other,
        };
        self.insert(id, recipe);
    }

    /// Recipe ids relevant to `book_type` (see [`Recipe::book_type`]),
    /// optionally narrowed to `category` (`None` is vanilla's "search" tab —
    /// every category) and a case-insensitive substring `search` — matched
    /// against the result item's namespaced id *and* its bare path (so
    /// `"planks"` matches `minecraft:oak_planks` without the namespace),
    /// never empty-string-vacuously (an empty `search` matches everything).
    ///
    /// Results come back in the corpus's own id order (`recipes` is kept
    /// id-sorted — see the type docs' "Ordering"), **not** vanilla's real
    /// fuzzy tooltip/name search tree (`ClientPacketListener.searchTrees()`),
    /// which needs the resolved item display name and tooltip text this
    /// crate does not have. This is a deliberate, documented simplification:
    /// substring-on-id, not word-fuzzy-on-display-name.
    #[must_use]
    pub fn browse(
        &self,
        book_type: RecipeBookType,
        category: Option<RecipeCategory>,
        search: &str,
    ) -> Vec<&Identifier> {
        let needle = search.to_ascii_lowercase();
        self.recipes
            .iter()
            .filter(|(_, r)| r.book_type() == Some(book_type))
            .filter(|(_, r)| category.is_none() || r.category() == category)
            .filter(|(_, r)| needle.is_empty() || Self::result_matches(r, &needle))
            .map(|(id, _)| id)
            .collect()
    }

    fn result_matches(recipe: &Recipe, needle: &str) -> bool {
        let Some(id) = recipe.result_item() else {
            return false;
        };
        id.path().to_ascii_lowercase().contains(needle) || id.to_string().to_ascii_lowercase().contains(needle)
    }

    /// The subset of [`tabs_for`] that actually has at least one loaded
    /// recipe for `book_type` — vanilla's `RecipeBookTabButton::updateVisibility`
    /// (`RecipeBookTabButton.java:88-100`): a tab with zero matching recipes
    /// never renders at all, rather than rendering empty.
    #[must_use]
    pub fn visible_tabs(&self, book_type: RecipeBookType) -> Vec<RecipeCategory> {
        tabs_for(book_type)
            .iter()
            .copied()
            .filter(|cat| {
                self.recipes
                    .iter()
                    .any(|(_, r)| r.book_type() == Some(book_type) && r.category() == Some(*cat))
            })
            .collect()
    }
}

/// One step of an auto-fill plan (issue #163, "click recipe to auto-fill"):
/// move one item from inventory slot `source_slot` into grid cell `cell`.
///
/// `cell` is a **0-based, row-major index into the placement grid**
/// ([`Recipe::placement`]'s own indexing), not yet a menu-slot index —
/// [`crate::menu::Menu::plan_recipe_auto_fill`] is what adds the menu's own
/// `craft_layout().first_input` offset. `source_slot` **is** already an
/// absolute menu-slot index, since that is what the caller's inventory
/// snapshot was keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementStep {
    /// 0-based row-major grid cell.
    pub cell: usize,
    /// Absolute menu-slot index of the inventory slot supplying it.
    pub source_slot: usize,
}

/// Computes an auto-fill plan for `recipe` against a `grid_w × grid_h`
/// crafting grid (or a furnace-family menu's `1×1` ingredient slot — see
/// [`CookingRecipe::placement`]), given the player's available inventory as
/// `(menu-slot index, stack)` pairs.
///
/// For each grid cell the placement requires an ingredient, this greedily
/// takes the **first** inventory entry (in the order given) whose item
/// matches and still has an undrawn unit, decrementing its remaining count so
/// the same physical stack cannot supply two cells beyond what it actually
/// holds. This models "place one set of ingredients" only — not vanilla's
/// `use_max_items` (shift-click) multiplier, which spreads across however
/// many complete sets the inventory can supply; that is a documented scope
/// reduction, not an oversight.
///
/// Returns `None` if the recipe has no placement for this grid size, **or**
/// if any single required ingredient has no matching inventory entry at
/// all — a conservative all-or-nothing plan, so a caller never ends up
/// partially filling a grid it cannot complete.
#[must_use]
pub fn plan_auto_fill(
    recipe: &Recipe,
    grid_w: usize,
    grid_h: usize,
    inventory: &[(usize, &ItemStack)],
    tags: &TagResolver,
) -> Option<Vec<PlacementStep>> {
    let placement = recipe.placement(grid_w, grid_h)?;
    let mut remaining: Vec<i32> = inventory.iter().map(|(_, s)| s.count()).collect();
    let mut steps = Vec::new();
    for (cell, ing) in placement.iter().enumerate() {
        let Some(ing) = ing else { continue };
        let found = inventory.iter().enumerate().find(|(i, (_, stack))| {
            remaining[*i] > 0 && ing.matches(stack.item(), tags)
        });
        let (i, (slot, _)) = found?;
        remaining[i] -= 1;
        steps.push(PlacementStep {
            cell,
            source_slot: *slot,
        });
    }
    Some(steps)
}

/// Tracks which recipes the server has told this client are unlocked
/// (issue #163, "recipe-unlock tracking"), plus which of those are still
/// "new" (unseen — vanilla's highlight squeeze animation and toast).
///
/// **Nothing populates this yet.** The server signal is vanilla's
/// `recipe_book_add`/`recipe_book_remove` packets (`RecipeBookAddPacket`/
/// `RecipeBookRemovePacket`), and `crates/protocol/v770/src/adapter.rs`
/// decodes neither — confirmed by grepping the packet-id constants
/// (`RECIPE_BOOK_ADD`/`RECIPE_BOOK_REMOVE`) against `adapter.rs`, zero hits —
/// nor is there a `ClientEvent` variant for them in `lodestone-model` to
/// decode *into*. Both are outside this issue's owned files (`crates/
/// protocol/**` is off-limits to this change; see `docs/crafting.md`).
///
/// Until that lands, [`is_unlocked`](Self::is_unlocked) reports every recipe
/// as unlocked (see its own doc comment) so the browsable panel shows the
/// full local corpus rather than nothing — a **visible, honestly degraded**
/// stand-in, not a silent fake. [`unlock`](Self::unlock) and
/// [`take_new`](Self::take_new) are real and unit-tested against direct
/// calls; they are simply never called by anything today.
#[derive(Debug, Default, Clone)]
pub struct RecipeUnlockState {
    /// Recipes the server has explicitly unlocked, once real data arrives.
    /// Empty on every session today — see the type doc.
    known: HashSet<Identifier>,
    /// Unlocked recipes not yet shown to the player (`recipeShown`,
    /// `RecipeBookComponent.java:533-535`) — drives the toast and the tab
    /// squeeze-highlight animation.
    new: HashSet<Identifier>,
    /// Whether [`unlock`](Self::unlock) or [`remove`](Self::remove) has ever
    /// been called. **Deliberately not derived from `known.is_empty()`**: an
    /// unlock immediately followed by a remove (or a server that unlocks
    /// then un-learns the same recipe, e.g. a datapack reload) would empty
    /// `known` again and — without this flag — incorrectly fall back into
    /// [`is_unlocked`](Self::is_unlocked)'s "no data yet" degrade, silently
    /// re-showing every recipe as unlocked after real data had already
    /// narrowed it down. A test pins exactly this sequence.
    has_data: bool,
}

impl RecipeUnlockState {
    /// A state with no unlock data at all.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `id` unlocked and unseen. Idempotent.
    pub fn unlock(&mut self, id: Identifier) {
        self.new.insert(id.clone());
        self.known.insert(id);
        self.has_data = true;
    }

    /// Reverses [`unlock`](Self::unlock) — vanilla's `recipe_book_remove`,
    /// sent when a recipe is un-learned (e.g. a datapack reload).
    pub fn remove(&mut self, id: &Identifier) {
        self.known.remove(id);
        self.new.remove(id);
        self.has_data = true;
    }

    /// Whether `id` should currently show as unlocked in the panel.
    ///
    /// **Degrades to "yes, always"** while this state has never received a
    /// single real unlock/removal signal (see the type doc and
    /// [`has_data`](Self::has_data)) — a browsable panel that always reports
    /// every recipe locked, forever, on every server, would be a *dead*
    /// control masquerading as a real one. The moment a single real unlock
    /// or removal arrives this switches to the honest per-id answer,
    /// including reporting recipes *other than* the one just unlocked as
    /// locked.
    #[must_use]
    pub fn is_unlocked(&self, id: &Identifier) -> bool {
        !self.has_data || self.known.contains(id)
    }

    /// Whether any real unlock/removal has ever been recorded — the escape
    /// hatch a caller needs to tell "everything shown because we have no
    /// data" apart from "everything shown because everything really is
    /// unlocked", since [`is_unlocked`] cannot distinguish them by itself.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.has_data
    }

    /// Drains and returns every recipe marked "new" — call once per toast
    /// dispatch (see [`RecipeToastQueue`]) so each unlock notifies exactly
    /// once.
    pub fn take_new(&mut self) -> Vec<Identifier> {
        std::mem::take(&mut self.new).into_iter().collect()
    }
}

/// Vanilla's recipe-unlock toast timing (`RecipeToast.java`): one toast that
/// **merges** every recipe unlocked within its display window, cycling
/// through them, rather than stacking N separate toasts.
///
/// `DISPLAY_TIME = 5000L` milliseconds (`RecipeToast.java:17`) is **100
/// ticks** at the server's fixed 50ms/tick — not a round number of ticks by
/// coincidence, exactly like every other vanilla UI timing keyed off
/// `System.currentTimeMillis()` rather than tick count.
pub const RECIPE_TOAST_DISPLAY_MS: u64 = 5000;
/// Vanilla's toast width in GUI pixels (`Toast.DEFAULT_WIDTH`, `Toast.java:14`).
pub const RECIPE_TOAST_WIDTH: u32 = 160;
/// Vanilla's toast height in GUI pixels (`Toast.SLOT_HEIGHT`, `Toast.java:15`).
pub const RECIPE_TOAST_HEIGHT: u32 = 32;

/// A pending recipe-unlock toast: which recipes to show and when the current
/// display window started. Pure timing data — no rendering, no clock of its
/// own; the caller supplies "now" so this stays deterministic and testable.
///
/// See [`RecipeUnlockState`]'s doc comment: this queue is exercised directly
/// by its own tests, but nothing yet calls [`push`](Self::push) from live
/// server data — the decode it depends on does not exist yet either.
#[derive(Debug, Default, Clone)]
pub struct RecipeToastQueue {
    /// `(crafting-station item, unlocked item)` pairs — mirrors
    /// `RecipeToast.Entry` (`RecipeToast.java:85-86`); the station item is
    /// the small corner icon (a crafting table, furnace, etc.).
    entries: Vec<(Identifier, Identifier)>,
    /// Milliseconds timestamp (caller's clock) the display window last reset.
    last_changed_ms: u64,
}

impl RecipeToastQueue {
    /// An empty, hidden queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an unlocked recipe and resets the display window — vanilla's
    /// `RecipeToast.addItem` setting `changed = true`
    /// (`RecipeToast.java:67-70`), which `update` reads back into
    /// `lastChanged` on the *next* frame. Modelled here as an immediate
    /// reset since this queue has no separate "changed" flag to defer
    /// through.
    pub fn push(&mut self, station: Identifier, unlocked: Identifier, now_ms: u64) {
        self.entries.push((station, unlocked));
        self.last_changed_ms = now_ms;
    }

    /// Whether the toast should currently be visible: non-empty and still
    /// within [`RECIPE_TOAST_DISPLAY_MS`] of the last reset
    /// (`RecipeToast.java:44-46`).
    #[must_use]
    pub fn visible(&self, now_ms: u64) -> bool {
        !self.entries.is_empty() && now_ms.saturating_sub(self.last_changed_ms) < RECIPE_TOAST_DISPLAY_MS
    }

    /// Which entry should be showing right now, cycling through every pending
    /// unlock over the display window — `RecipeToast.java:49-51`'s
    /// `displayedRecipeIndex` formula, with the notification-time multiplier
    /// fixed at `1.0` (this client has no accessibility "toast display time"
    /// option to read).
    #[must_use]
    pub fn displayed_entry(&self, now_ms: u64) -> Option<(&Identifier, &Identifier)> {
        if self.entries.is_empty() {
            return None;
        }
        let elapsed = now_ms.saturating_sub(self.last_changed_ms);
        let per_entry = (RECIPE_TOAST_DISPLAY_MS as f64 / self.entries.len() as f64).max(1.0);
        let index = ((elapsed as f64 / per_entry) as usize) % self.entries.len();
        self.entries.get(index).map(|(a, b)| (a, b))
    }

    /// Clears every pending entry, hiding the toast immediately.
    pub fn clear(&mut self) {
        self.entries.clear();
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

/// The server's stored per-book recipe-book UI state — open, and filtering — for
/// each of the four books.
///
/// # What it is
///
/// The fold behind [`ClientEvent::RecipeBookSettingsChanged`]. Vanilla persists
/// each book's open/filter state per player and replays it on join, which is why
/// re-opening a crafting table remembers that you had the filter on.
///
/// # Why it existed only in the outbound direction until now
///
/// `ClientAction::SetRecipeBookSettings` has been encoded by the adapters for some
/// time, so the client could *tell* the server its book state. The clientbound
/// `RECIPE_BOOK_SETTINGS` packet had no decode at all — the packet id was
/// registered, which proves only that the id is known, and
/// `cargo xtask connectedness` counted it as undecoded. So the round trip was
/// half-open: our state could go out and the server's could never come back.
///
/// # How to change it
///
/// [`Self::for_type`] is the read accessor; keep the wire order (`crafting`,
/// `furnace`, `blast_furnace`, `smoker`) if you index positionally, because that
/// order is fixed by `RecipeBookSettings.STREAM_CODEC` and is not alphabetical.
///
/// `reported` exists for the same reason `SpawnPoint`'s `Option` does: a server
/// that has never sent this packet is not the same thing as a server that sent
/// all-`false`, and a UI that wants to distinguish "closed" from "unknown" needs
/// the difference. Unlike a bare `Option` this keeps the per-book values usable
/// without unwrapping, since vanilla's own default *is* all-`false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecipeBookSettings {
    /// The crafting-table book.
    pub crafting: RecipeBookTypeSettings,
    /// The furnace book.
    pub furnace: RecipeBookTypeSettings,
    /// The blast-furnace book.
    pub blast_furnace: RecipeBookTypeSettings,
    /// The smoker book.
    pub smoker: RecipeBookTypeSettings,
    /// Whether the server has ever reported these settings.
    pub reported: bool,
}

impl RecipeBookSettings {
    /// Fold one event, returning whether it belonged to this aggregate.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::RecipeBookSettingsChanged {
                crafting,
                furnace,
                blast_furnace,
                smoker,
            } => {
                // Assigned as a whole record: one packet reports all four books
                // together, so there is no way to hold a stale furnace state next
                // to a fresh crafting one.
                self.crafting = *crafting;
                self.furnace = *furnace;
                self.blast_furnace = *blast_furnace;
                self.smoker = *smoker;
                self.reported = true;
                true
            }
            _ => false,
        }
    }

    /// The settings for one book.
    #[must_use]
    pub fn for_type(&self, book_type: RecipeBookType) -> RecipeBookTypeSettings {
        match book_type {
            RecipeBookType::Crafting => self.crafting,
            RecipeBookType::Furnace => self.furnace,
            RecipeBookType::BlastFurnace => self.blast_furnace,
            RecipeBookType::Smoker => self.smoker,
        }
    }
}

#[cfg(test)]
mod recipe_book_settings_tests {
    use super::*;

    fn event(
        pairs: [(bool, bool); 4],
    ) -> ClientEvent {
        let s = |(open, filtering): (bool, bool)| RecipeBookTypeSettings { open, filtering };
        ClientEvent::RecipeBookSettingsChanged {
            crafting: s(pairs[0]),
            furnace: s(pairs[1]),
            blast_furnace: s(pairs[2]),
            smoker: s(pairs[3]),
        }
    }

    #[test]
    fn starts_unreported_with_vanillas_all_false_default() {
        let s = RecipeBookSettings::default();
        assert!(!s.reported, "unreported must be distinguishable from all-false");
        for t in [
            RecipeBookType::Crafting,
            RecipeBookType::Furnace,
            RecipeBookType::BlastFurnace,
            RecipeBookType::Smoker,
        ] {
            assert_eq!(s.for_type(t), RecipeBookTypeSettings::default());
        }
    }

    /// `for_type` must map each book to *its own* pair. A wrong mapping is the
    /// available mistake and an all-books-identical fixture could not catch it, so
    /// every one of the four pairs here is distinct.
    #[test]
    fn for_type_maps_each_book_to_its_own_pair() {
        let mut s = RecipeBookSettings::default();
        assert!(s.apply(&event([
            (true, false),
            (false, true),
            (true, true),
            (false, false),
        ])));
        assert!(s.reported);
        assert_eq!(
            s.for_type(RecipeBookType::Crafting),
            RecipeBookTypeSettings { open: true, filtering: false }
        );
        assert_eq!(
            s.for_type(RecipeBookType::Furnace),
            RecipeBookTypeSettings { open: false, filtering: true }
        );
        assert_eq!(
            s.for_type(RecipeBookType::BlastFurnace),
            RecipeBookTypeSettings { open: true, filtering: true }
        );
        assert_eq!(
            s.for_type(RecipeBookType::Smoker),
            RecipeBookTypeSettings { open: false, filtering: false }
        );
    }

    /// Negative control for the `_ => false` arm.
    #[test]
    fn unrelated_events_are_rejected_and_change_nothing() {
        let mut s = RecipeBookSettings::default();
        assert!(!s.apply(&ClientEvent::KeepAlive { id: 1 }));
        assert!(
            !s.reported,
            "an unrelated event must not mark the settings reported"
        );
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
                category: RecipeCategory::Misc,
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

    // -- Recipe-book browsing (issue #163) -----------------------------

    /// A small crafting-book corpus, deliberately with a **non-alphabetical**
    /// id order relative to the result item name, so a search-order test can
    /// tell "corpus id order" apart from "alphabetical by result" instead of
    /// the two hypotheses coinciding by accident.
    fn browse_book() -> RecipeBook {
        let mut book = RecipeBook::new();
        // id `minecraft:zz_planks_wall`, result `oak_planks` — sorts LAST by
        // id but would sort FIRST alphabetically by its result name.
        book.insert(
            id("minecraft:zz_planks_wall"),
            Recipe::Shaped(
                ShapedRecipe::new(
                    1,
                    1,
                    vec![Some(Ingredient::Item(id("minecraft:oak_planks")))],
                    stack("minecraft:oak_planks", 1),
                )
                .with_category(RecipeCategory::Building),
            ),
        );
        // id `minecraft:aa_torch` — sorts FIRST by id, result `oak_planks`
        // path also contains "planks".
        book.insert(
            id("minecraft:aa_torch"),
            Recipe::Shapeless(
                ShapelessRecipe::new(
                    vec![Ingredient::Item(id("minecraft:oak_planks"))],
                    stack("minecraft:torch", 4),
                )
                .with_category(RecipeCategory::Misc),
            ),
        );
        // A redstone-category recipe with no "planks" in its result, to
        // prove the search narrows rather than just listing the category.
        book.insert(
            id("minecraft:mm_dropper"),
            Recipe::Shaped(
                ShapedRecipe::new(
                    1,
                    1,
                    vec![Some(Ingredient::Item(id("minecraft:cobblestone")))],
                    stack("minecraft:dropper", 1),
                )
                .with_category(RecipeCategory::Redstone),
            ),
        );
        // A furnace recipe, to prove `browse` narrows by book type too.
        book.insert(
            id("minecraft:cooked_porkchop"),
            Recipe::Cooking(CookingRecipe {
                kind: CookingKind::Smelting,
                ingredient: Ingredient::Item(id("minecraft:porkchop")),
                result: stack("minecraft:cooked_porkchop", 1),
                experience: 0.35,
                cooking_time: 200,
                category: RecipeCategory::Food,
            }),
        );
        book
    }

    /// Predicts the exact ordered id list `browse` returns for a corpus
    /// where id order and "alphabetical by result item name" **disagree**:
    /// `minecraft:aa_first` (sorts first by id) produces `minecraft:zz_oak_
    /// planks`, while `minecraft:zz_second` (sorts last by id) produces
    /// `minecraft:aa_birch_planks` — alphabetically `aa_birch_planks` <
    /// `zz_oak_planks`, so the **rejected** "alphabetical by result name"
    /// hypothesis would return `[zz_second, aa_first]`. `browse` searches on
    /// the *result* item's id (`result_item`, not the ingredients), so both
    /// match `"planks"`; the actual, correct order is the corpus's own id
    /// order: `aa_first` then `zz_second`.
    #[test]
    fn browse_search_orders_by_corpus_id_not_alphabetically_by_result() {
        let mut book = RecipeBook::new();
        book.insert(
            id("minecraft:aa_first"),
            Recipe::Shaped(ShapedRecipe::new(
                1,
                1,
                vec![Some(Ingredient::Item(id("minecraft:oak_log")))],
                stack("minecraft:zz_oak_planks", 4),
            )),
        );
        book.insert(
            id("minecraft:zz_second"),
            Recipe::Shaped(ShapedRecipe::new(
                1,
                1,
                vec![Some(Ingredient::Item(id("minecraft:birch_log")))],
                stack("minecraft:aa_birch_planks", 4),
            )),
        );
        let got: Vec<String> = book
            .browse(RecipeBookType::Crafting, None, "planks")
            .into_iter()
            .map(ToString::to_string)
            .collect();
        let rejected_alphabetical_by_result: Vec<String> =
            vec!["minecraft:zz_second".to_string(), "minecraft:aa_first".to_string()];
        assert_eq!(
            got,
            vec!["minecraft:aa_first".to_string(), "minecraft:zz_second".to_string()]
        );
        assert_ne!(got, rejected_alphabetical_by_result);
    }

    #[test]
    fn browse_narrows_by_book_type_and_category() {
        let book = browse_book();
        assert_eq!(
            book.browse(RecipeBookType::Crafting, Some(RecipeCategory::Redstone), ""),
            vec![&id("minecraft:mm_dropper")]
        );
        assert_eq!(
            book.browse(RecipeBookType::Furnace, None, ""),
            vec![&id("minecraft:cooked_porkchop")]
        );
        assert_eq!(book.browse(RecipeBookType::BlastFurnace, None, ""), Vec::<&Identifier>::new());
    }

    #[test]
    fn browse_empty_search_matches_everything_in_scope() {
        let book = browse_book();
        assert_eq!(book.browse(RecipeBookType::Crafting, None, "").len(), 3);
    }

    /// `tabs_for` is vanilla's own declaration order
    /// (`RecipeBookCategories.java:7-19`), not alphabetical. Pinning the
    /// asymmetric cases explicitly: `BlastFurnace` has no `Food` tab and
    /// `Smoker` has only `Food`.
    #[test]
    fn tabs_for_matches_vanillas_declaration_order() {
        assert_eq!(
            tabs_for(RecipeBookType::Crafting),
            &[
                RecipeCategory::Building,
                RecipeCategory::Redstone,
                RecipeCategory::Equipment,
                RecipeCategory::Misc
            ]
        );
        assert_eq!(
            tabs_for(RecipeBookType::Furnace),
            &[RecipeCategory::Food, RecipeCategory::Blocks, RecipeCategory::Misc]
        );
        assert_eq!(
            tabs_for(RecipeBookType::BlastFurnace),
            &[RecipeCategory::Blocks, RecipeCategory::Misc]
        );
        assert_eq!(tabs_for(RecipeBookType::Smoker), &[RecipeCategory::Food]);
    }

    /// A tab with zero loaded recipes must not appear — `visible_tabs` is
    /// `tabs_for` filtered, not `tabs_for` restated.
    #[test]
    fn visible_tabs_omits_empty_categories() {
        let book = browse_book();
        // Crafting has Building + Redstone + Misc recipes loaded, but no
        // Equipment recipe at all in this corpus.
        assert_eq!(
            book.visible_tabs(RecipeBookType::Crafting),
            vec![RecipeCategory::Building, RecipeCategory::Redstone, RecipeCategory::Misc]
        );
    }

    #[test]
    fn recipe_json_category_field_parses_to_the_right_variant() {
        assert_eq!(RecipeCategory::from_json_str("building"), RecipeCategory::Building);
        assert_eq!(RecipeCategory::from_json_str("redstone"), RecipeCategory::Redstone);
        assert_eq!(RecipeCategory::from_json_str("equipment"), RecipeCategory::Equipment);
        assert_eq!(RecipeCategory::from_json_str("food"), RecipeCategory::Food);
        assert_eq!(RecipeCategory::from_json_str("blocks"), RecipeCategory::Blocks);
        assert_eq!(RecipeCategory::from_json_str("nonsense"), RecipeCategory::Misc);
    }

    // -- Auto-fill planning (issue #163, "click recipe to auto-fill") --

    #[test]
    fn shaped_placement_is_top_left_unmirrored() {
        // "X " / " #" in a 2x2 pattern.
        let recipe = ShapedRecipe::new(
            2,
            1,
            vec![
                Some(Ingredient::Item(id("minecraft:stick"))),
                Some(Ingredient::Item(id("minecraft:coal"))),
            ],
            stack("minecraft:torch", 4),
        );
        let placement = recipe.placement(3, 3).expect("fits a 3x3 grid");
        // Row-major 3x3: cell 0 = (0,0) = stick, cell 1 = (1,0) = coal, the
        // rest empty — top-left, never mirrored (mirrored would put coal
        // first).
        assert_eq!(placement[0], Some(&Ingredient::Item(id("minecraft:stick"))));
        assert_eq!(placement[1], Some(&Ingredient::Item(id("minecraft:coal"))));
        assert_eq!(placement[2], None);
        assert!(placement[3..].iter().all(Option::is_none));
    }

    #[test]
    fn shapeless_placement_fills_left_to_right_top_to_bottom() {
        let recipe = ShapelessRecipe::new(
            vec![
                Ingredient::Item(id("minecraft:coal")),
                Ingredient::Item(id("minecraft:stick")),
            ],
            stack("minecraft:torch", 4),
        );
        let placement = recipe.placement(3, 3).unwrap();
        assert_eq!(placement[0], Some(&Ingredient::Item(id("minecraft:coal"))));
        assert_eq!(placement[1], Some(&Ingredient::Item(id("minecraft:stick"))));
        assert!(placement[2..].iter().all(Option::is_none));
    }

    #[test]
    fn cooking_placement_is_a_single_cell_at_1x1_only() {
        let recipe = CookingRecipe {
            kind: CookingKind::Smelting,
            ingredient: Ingredient::Item(id("minecraft:porkchop")),
            result: stack("minecraft:cooked_porkchop", 1),
            experience: 0.35,
            cooking_time: 200,
            category: RecipeCategory::Food,
        };
        assert_eq!(
            recipe.placement(1, 1),
            Some(vec![Some(&Ingredient::Item(id("minecraft:porkchop")))])
        );
        assert_eq!(recipe.placement(3, 3), None);
    }

    /// Predicts the exact plan for a torch (`stick` + `coal`/`charcoal`) in a
    /// 3x3 grid, with the player holding coal at slot 12 and sticks at slot
    /// 20. Cell 0 (stick) must draw from slot 20, cell 1 (coal) from slot
    /// 12 — **not** id order, source order: the rejected hypothesis "lowest
    /// slot index first regardless of which cell needs it" would still pick
    /// slot 12 before 20, but would assign it to whichever cell is checked
    /// first that it satisfies, which happens to coincide here only because
    /// cell 1 needs coal — so the second case below (swapped inventory
    /// order) is the one that actually distinguishes the hypotheses.
    #[test]
    fn plan_auto_fill_predicts_exact_source_slots_for_a_torch() {
        let torch = Recipe::Shaped(ShapedRecipe::new(
            1,
            2,
            vec![
                Some(Ingredient::Item(id("minecraft:coal"))),
                Some(Ingredient::Item(id("minecraft:stick"))),
            ],
            stack("minecraft:torch", 4),
        ));
        let coal = stack("minecraft:coal", 5);
        let stick = stack("minecraft:stick", 3);
        let inventory = [(12usize, &coal), (20usize, &stick)];
        let tags = TagResolver::new();
        let plan = plan_auto_fill(&torch, 1, 2, &inventory, &tags).expect("both ingredients present");
        assert_eq!(
            plan,
            vec![
                PlacementStep { cell: 0, source_slot: 12 },
                PlacementStep { cell: 1, source_slot: 20 },
            ]
        );
    }

    /// The same recipe with the coal stack **exhausted** (one coal, but two
    /// cells that could each match "coal or a coal-like tag" is not this
    /// recipe's shape — instead this pins the *all-or-nothing* behaviour: a
    /// recipe needing an item the inventory does not have at all returns
    /// `None`, not a partial plan.
    #[test]
    fn plan_auto_fill_is_all_or_nothing() {
        let torch = Recipe::Shaped(ShapedRecipe::new(
            1,
            2,
            vec![
                Some(Ingredient::Item(id("minecraft:coal"))),
                Some(Ingredient::Item(id("minecraft:stick"))),
            ],
            stack("minecraft:torch", 4),
        ));
        let coal = stack("minecraft:coal", 5);
        // No sticks anywhere in inventory.
        let inventory = [(12usize, &coal)];
        let tags = TagResolver::new();
        assert_eq!(plan_auto_fill(&torch, 1, 2, &inventory, &tags), None);
    }

    /// A single stack cannot supply two cells beyond the units it actually
    /// holds: with exactly one stick, a recipe needing two sticks fails
    /// rather than reusing the same slot twice.
    #[test]
    fn plan_auto_fill_does_not_overdraw_a_single_stack() {
        let two_sticks = Recipe::Shapeless(ShapelessRecipe::new(
            vec![
                Ingredient::Item(id("minecraft:stick")),
                Ingredient::Item(id("minecraft:stick")),
            ],
            stack("minecraft:test_result", 1),
        ));
        let stick = stack("minecraft:stick", 1);
        let inventory = [(20usize, &stick)];
        let tags = TagResolver::new();
        assert_eq!(plan_auto_fill(&two_sticks, 1, 2, &inventory, &tags), None);
    }

    // -- RecipeUnlockState (issue #163, "recipe-unlock tracking") -------

    #[test]
    fn unlock_state_degrades_to_everything_unlocked_with_no_data() {
        let state = RecipeUnlockState::new();
        assert!(!state.has_data());
        assert!(state.is_unlocked(&id("minecraft:anything_at_all")));
    }

    #[test]
    fn unlock_state_switches_to_honest_per_id_answers_after_first_real_signal() {
        let mut state = RecipeUnlockState::new();
        state.unlock(id("minecraft:torch"));
        assert!(state.has_data());
        assert!(state.is_unlocked(&id("minecraft:torch")));
        // A different, never-unlocked recipe now correctly reports locked —
        // the whole point of leaving the always-unlocked degrade behind.
        assert!(!state.is_unlocked(&id("minecraft:diamond_pickaxe")));
    }

    #[test]
    fn unlock_state_new_highlight_is_taken_exactly_once() {
        let mut state = RecipeUnlockState::new();
        state.unlock(id("minecraft:torch"));
        let first = state.take_new();
        assert_eq!(first, vec![id("minecraft:torch")]);
        assert_eq!(state.take_new(), Vec::<Identifier>::new());
    }

    #[test]
    fn unlock_state_remove_reverses_unlock() {
        let mut state = RecipeUnlockState::new();
        state.unlock(id("minecraft:torch"));
        state.remove(&id("minecraft:torch"));
        assert!(state.has_data());
        assert!(!state.is_unlocked(&id("minecraft:torch")));
    }

    // -- RecipeToastQueue (issue #163, "unlock toast notification") -----

    #[test]
    fn toast_queue_hidden_when_empty() {
        let queue = RecipeToastQueue::new();
        assert!(!queue.visible(0));
        assert_eq!(queue.displayed_entry(0), None);
    }

    /// `RECIPE_TOAST_DISPLAY_MS` is vanilla's `5000` (`RecipeToast.java:17`).
    /// Visible strictly before the 5000ms mark, hidden at and after it.
    #[test]
    fn toast_queue_visible_window_is_exactly_5000ms() {
        let mut queue = RecipeToastQueue::new();
        queue.push(id("minecraft:crafting_table"), id("minecraft:torch"), 1_000);
        assert!(queue.visible(1_000));
        assert!(queue.visible(1_000 + RECIPE_TOAST_DISPLAY_MS - 1));
        assert!(!queue.visible(1_000 + RECIPE_TOAST_DISPLAY_MS));
    }

    /// Two entries over the 5000ms window cycle at the 2500ms midpoint —
    /// `RecipeToast.java:49-51`'s formula with `manager.getNotificationDisplayTimeMultiplier() == 1.0`.
    #[test]
    fn toast_queue_cycles_entries_at_the_predicted_midpoint() {
        let mut queue = RecipeToastQueue::new();
        queue.push(id("minecraft:crafting_table"), id("minecraft:torch"), 0);
        queue.push(id("minecraft:furnace"), id("minecraft:cooked_porkchop"), 0);
        assert_eq!(
            queue.displayed_entry(0),
            Some((&id("minecraft:crafting_table"), &id("minecraft:torch")))
        );
        assert_eq!(
            queue.displayed_entry(2_499),
            Some((&id("minecraft:crafting_table"), &id("minecraft:torch")))
        );
        assert_eq!(
            queue.displayed_entry(2_500),
            Some((&id("minecraft:furnace"), &id("minecraft:cooked_porkchop")))
        );
    }

    #[test]
    fn toast_queue_clear_hides_immediately() {
        let mut queue = RecipeToastQueue::new();
        queue.push(id("minecraft:crafting_table"), id("minecraft:torch"), 0);
        queue.clear();
        assert!(!queue.visible(0));
    }
}
