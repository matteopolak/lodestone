//! Furnace family block entity: burn/cook state machine shared by the
//! furnace, smoker, and blast furnace (issue #251).
//!
//! # Where the truth comes from
//!
//! `.cache/mc/26.2/src/net/minecraft/world/level/block/entity/
//! AbstractFurnaceBlockEntity.java` (the shared engine; `FurnaceBlockEntity`/
//! `SmokerBlockEntity`/`BlastFurnaceBlockEntity` are thin subclasses that
//! only pick a [`FuelValues`]-halving strategy and a recipe type — see
//! below), `FuelValues.java` (fuel burn durations), and the four
//! `AbstractCookingRecipe` subclasses (`SmeltingRecipe.java`,
//! `SmokingRecipe.java`, `BlastingRecipe.java`) for the default cook time per
//! recipe kind. The concrete recipe table below (ingredient -> result/count/
//! experience/cooking_time) is transcribed mechanically from every
//! `minecraft:smelting`/`minecraft:blasting`/`minecraft:smoking` recipe JSON
//! under `.cache/mc/26.2/client-src/data/minecraft/recipe/` (Mojang's own
//! generated data — data source #1 per `CLAUDE.md`), not hand-typed from
//! memory or a wiki. `minecraft:campfire_cooking` recipes are excluded: a
//! campfire has no fuel/lit state machine at all (no block entity — vanilla
//! ticks it directly off `CampfireBlock`), so it is not part of this
//! "furnace family" issue's scope.
//!
//! ## The state machine (`AbstractFurnaceBlockEntity.serverTick`,
//! `:139-207`)
//!
//! Quoted here because [`Furnace::tick`] is a direct, line-by-line port:
//!
//! ```java
//! if (entity.litTimeRemaining > 0) {
//!     wasLit = true;
//!     entity.litTimeRemaining--;
//!     isLit = entity.litTimeRemaining > 0;
//! } else {
//!     wasLit = false;
//!     isLit = false;
//! }
//! // ... hasIngredient/hasFuel from slots 0/1 ...
//! if (isLit || hasFuel && hasIngredient) {
//!     if (hasIngredient) {
//!         // look up recipe for slot 0; if found and canBurn(...):
//!         //   if (!isLit) { light from fuel; consume 1 fuel if burn time > 0; }
//!         //   if (isLit) { cookingTimer++; if (== cookingTotalTime) { reset, produce, setRecipeUsed }; }
//!         //   else { cookingTimer = 0; }
//!         // else (canBurn false): cookingTimer = 0;
//!         // (recipe == null: cookingTimer is left untouched — a real vanilla quirk, preserved here)
//!     } else {
//!         cookingTimer = 0;
//!     }
//! } else if (cookingTimer > 0) {
//!     cookingTimer = clamp(cookingTimer - BURN_COOL_SPEED, 0, cookingTotalTime);
//! }
//! ```
//!
//! `cookingTotalTime` is **not** recomputed every tick — it is fixed the
//! moment the input slot changes to a different item (`setItem`,
//! `:291-302`: `slot == 0 && !same` recomputes it from the new recipe, or
//! `200` if none), which is why the produced item's cook time cannot change
//! mid-cook even if the recipe table were hot-swapped. [`Furnace::set_input`]
//! mirrors that.
//!
//! ## Fuel: `FuelValues.java`
//!
//! `baseUnit = 200` (`vanillaBurnTimes`, `:38-40`). [`base_burn_duration`]
//! restates every concrete `.add(ItemLike, ...)` entry
//! (`FuelValues.java:44-107`) plus the flammable-wood-family tags
//! (`logs_that_burn`/`planks`/`wooden_slabs`/`wooden_stairs`/`wooden_doors`/
//! `wooden_trapdoors`/`wooden_fences`/`fence_gates`/
//! `wooden_pressure_plates`/`wooden_buttons`/`signs`/`hanging_signs`/
//! `boats`/`wooden_shelves`, each `.cache/mc/26.2/client-src/data/minecraft/
//! tags/item/*.json`) for the nine flammable overworld wood species (oak,
//! spruce, birch, jungle, acacia, dark_oak, pale_oak, mangrove, cherry) plus
//! bamboo for the tags that include it, with `crimson_`/`warped_` explicitly
//! excluded — `NON_FLAMMABLE_WOOD` (`non_flammable_wood.json`) is exactly
//! those two prefixes, verified directly rather than assumed
//! (`FuelValues.java:108`, `.remove(ItemTags.NON_FLAMMABLE_WOOD)`). Wool
//! (`ItemTags.WOOL`, any of the 16 dye colours, `:85`) and wool carpets
//! (`:90`) and banners (`:71`) are matched by suffix for the same reason —
//! every colour is flammable, there is no exclusion tag for them. Smoker and
//! blast furnace halve the *fuel* burn duration, not the cook time
//! (`SmokerBlockEntity`/`BlastFurnaceBlockEntity` override `getBurnDuration`
//! to `return super.getBurnDuration(...) / 2;`) — modeled as
//! [`Furnace::effective_burn_duration`]'s per-[`FurnaceKind`] halving,
//! **separate** from the recipe table's own per-kind cook time (which is
//! independently already 100 vs 200 in the JSON — see the recipe doc above).
//!
//! Not modeled, documented rather than silently wrong: item-specific max
//! stack size for `canBurn`'s output-space check (every recipe result this
//! table produces stacks to 64, the assumed default — see [`MAX_STACK_SIZE`]);
//! the wet-sponge + bucket -> water-bucket special case
//! (`AbstractFurnaceBlockEntity.java:241-243`); and any fuel item's crafting
//! remainder (e.g. a lava bucket leaving an empty bucket behind,
//! `consumeFuel`, `:209-216`) — this crate's `ItemStack` has no
//! `ItemStackTemplate`/crafting-remainder registry to resolve one from, the
//! same gap `PlayerInventory`'s own docs note elsewhere.

use std::collections::HashMap;

use lodestone_model::ItemStack;

/// `AbstractFurnaceBlockEntity.BURN_COOL_SPEED` (`:55`): how fast cooking
/// progress decays per tick once the fire goes out with nothing left to
/// relight it.
pub const BURN_COOL_SPEED: i32 = 2;

/// The cook-time fallback when no recipe matches the current input
/// (`getTotalCookTime`'s `.orElse(200)`, `:254`).
pub const DEFAULT_COOK_TIME: i32 = 200;

/// The assumed output stack cap for [`Furnace::can_burn`] — see the module
/// doc comment's "not modeled" section for why this is a deliberate
/// simplification rather than a per-item lookup.
pub const MAX_STACK_SIZE: u32 = 64;

/// Which furnace-family block this is. Determines both the recipe table
/// consulted ([`recipe_for`]) and the fuel-burn-time halving
/// ([`Furnace::effective_burn_duration`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FurnaceKind {
    /// The plain furnace: `Smelting` recipes, full fuel duration.
    Furnace,
    /// `Smoking` recipes (food only), half fuel duration
    /// (`SmokerBlockEntity.getBurnDuration`).
    Smoker,
    /// `Blasting` recipes (ores/tools), half fuel duration
    /// (`BlastFurnaceBlockEntity.getBurnDuration`).
    BlastFurnace,
}

impl FurnaceKind {
    fn recipe_table_key(self) -> &'static str {
        match self {
            FurnaceKind::Furnace => "Smelting",
            FurnaceKind::Smoker => "Smoking",
            FurnaceKind::BlastFurnace => "Blasting",
        }
    }
}

/// One resolved cooking recipe: what a given input produces, and how long
/// it takes in *this* furnace kind's table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CookingRecipe {
    /// Output item id (`minecraft:...`).
    pub result: &'static str,
    /// Output stack count (always `1` in this table today, but modeled
    /// faithfully rather than assumed).
    pub count: u32,
    /// Experience banked per successful cook (paid out on collection, not
    /// per tick — see [`Furnace::take_recipes_used`]).
    pub experience: f32,
    /// Ticks to cook, already kind-specific (`200` for smelting, `100` for
    /// blasting/smoking per each recipe subclass's `cookingMapCodec`
    /// default).
    pub cooking_time: i32,
}

/// Looks up the cooking recipe for `ingredient` (a full `minecraft:...` id)
/// under `kind`'s recipe table. `None` means this item cannot be cooked at
/// all in this furnace kind (either not cookable anywhere, or only cookable
/// in a different kind's table — e.g. raw chicken is `Smoking`/
/// `CampfireCooking` only, never `Smelting`).
#[must_use]
pub fn recipe_for(kind: FurnaceKind, ingredient: &str) -> Option<CookingRecipe> {
    cooking_recipe(kind.recipe_table_key(), ingredient)
}

#[allow(clippy::too_many_lines)]
fn cooking_recipe(kind: &str, ingredient: &str) -> Option<CookingRecipe> {
    if let Some(recipe) = cooking_recipe_table(kind, ingredient) {
        return Some(recipe);
    }
    // `charcoal.json`'s ingredient is the tag `#minecraft:logs_that_burn`
    // (the same nine-species log/wood tag `base_burn_duration` already
    // resolves for fuel — `FLAMMABLE_WOOD_SPECIES`/`strip_species_suffix`,
    // defined below) — any log or wood block of a flammable species smelts
    // into charcoal. Modeled as a fallback rather than 36 literal arms
    // (9 species x {log, wood, stripped_log, stripped_wood}).
    if kind == "Smelting" && strip_species_suffix(ingredient, &["_log", "_wood"], FLAMMABLE_WOOD_SPECIES).is_some() {
        return Some(CookingRecipe {
            result: "minecraft:charcoal",
            count: 1,
            experience: 0.15,
            cooking_time: 200,
        });
    }
    None
}

#[allow(clippy::too_many_lines)]
fn cooking_recipe_table(kind: &str, ingredient: &str) -> Option<CookingRecipe> {
    match (kind, ingredient) {
        ("Blasting", "minecraft:ancient_debris") => Some(CookingRecipe { result: "minecraft:netherite_scrap", count: 1, experience: 2.0, cooking_time: 100 }),
        ("Blasting", "minecraft:chainmail_boots") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:chainmail_chestplate") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:chainmail_helmet") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:chainmail_leggings") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:coal_ore") => Some(CookingRecipe { result: "minecraft:coal", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_axe") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_boots") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_chestplate") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_helmet") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_hoe") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_horse_armor") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_leggings") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_nautilus_armor") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_ore") => Some(CookingRecipe { result: "minecraft:copper_ingot", count: 1, experience: 0.7, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_pickaxe") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_shovel") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_spear") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:copper_sword") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_coal_ore") => Some(CookingRecipe { result: "minecraft:coal", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_copper_ore") => Some(CookingRecipe { result: "minecraft:copper_ingot", count: 1, experience: 0.7, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_diamond_ore") => Some(CookingRecipe { result: "minecraft:diamond", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_emerald_ore") => Some(CookingRecipe { result: "minecraft:emerald", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_gold_ore") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_iron_ore") => Some(CookingRecipe { result: "minecraft:iron_ingot", count: 1, experience: 0.7, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_lapis_ore") => Some(CookingRecipe { result: "minecraft:lapis_lazuli", count: 1, experience: 0.2, cooking_time: 100 }),
        ("Blasting", "minecraft:deepslate_redstone_ore") => Some(CookingRecipe { result: "minecraft:redstone", count: 1, experience: 0.7, cooking_time: 100 }),
        ("Blasting", "minecraft:diamond_ore") => Some(CookingRecipe { result: "minecraft:diamond", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:emerald_ore") => Some(CookingRecipe { result: "minecraft:emerald", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:gold_ore") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_axe") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_boots") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_chestplate") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_helmet") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_hoe") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_horse_armor") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_leggings") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_nautilus_armor") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_pickaxe") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_shovel") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_spear") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:golden_sword") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_axe") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_boots") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_chestplate") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_helmet") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_hoe") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_horse_armor") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_leggings") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_nautilus_armor") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_ore") => Some(CookingRecipe { result: "minecraft:iron_ingot", count: 1, experience: 0.7, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_pickaxe") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_shovel") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_spear") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:iron_sword") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Blasting", "minecraft:lapis_ore") => Some(CookingRecipe { result: "minecraft:lapis_lazuli", count: 1, experience: 0.2, cooking_time: 100 }),
        ("Blasting", "minecraft:nether_gold_ore") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:nether_quartz_ore") => Some(CookingRecipe { result: "minecraft:quartz", count: 1, experience: 0.2, cooking_time: 100 }),
        ("Blasting", "minecraft:raw_copper") => Some(CookingRecipe { result: "minecraft:copper_ingot", count: 1, experience: 0.7, cooking_time: 100 }),
        ("Blasting", "minecraft:raw_gold") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 100 }),
        ("Blasting", "minecraft:raw_iron") => Some(CookingRecipe { result: "minecraft:iron_ingot", count: 1, experience: 0.7, cooking_time: 100 }),
        ("Blasting", "minecraft:redstone_ore") => Some(CookingRecipe { result: "minecraft:redstone", count: 1, experience: 0.7, cooking_time: 100 }),
        // `leaf_litter.json`'s ingredient is the tag `#minecraft:leaves`
        // (`leaves.json`) — expanded here to its concrete members since this
        // table is keyed by concrete item id, not tag reference.
        ("Smelting", "minecraft:jungle_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:oak_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:spruce_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:pale_oak_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:dark_oak_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:acacia_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:birch_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:azalea_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:flowering_azalea_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:mangrove_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        ("Smelting", "minecraft:cherry_leaves") => Some(CookingRecipe { result: "minecraft:leaf_litter", count: 1, experience: 0.1, cooking_time: 200 }), // leaf_litter.json, tag leaves.json
        // `glass.json`'s ingredient is the tag `#minecraft:smelts_to_glass`
        // (`smelts_to_glass.json` = sand + red sand only) — expanded for the
        // same reason as leaves above.
        ("Smelting", "minecraft:sand") => Some(CookingRecipe { result: "minecraft:glass", count: 1, experience: 0.1, cooking_time: 200 }), // glass.json, tag smelts_to_glass.json
        ("Smelting", "minecraft:red_sand") => Some(CookingRecipe { result: "minecraft:glass", count: 1, experience: 0.1, cooking_time: 200 }), // glass.json, tag smelts_to_glass.json
        ("Smelting", "minecraft:ancient_debris") => Some(CookingRecipe { result: "minecraft:netherite_scrap", count: 1, experience: 2.0, cooking_time: 200 }),
        ("Smelting", "minecraft:basalt") => Some(CookingRecipe { result: "minecraft:smooth_basalt", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:beef") => Some(CookingRecipe { result: "minecraft:cooked_beef", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:black_terracotta") => Some(CookingRecipe { result: "minecraft:black_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:blue_terracotta") => Some(CookingRecipe { result: "minecraft:blue_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:brown_terracotta") => Some(CookingRecipe { result: "minecraft:brown_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:cactus") => Some(CookingRecipe { result: "minecraft:green_dye", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:chainmail_boots") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:chainmail_chestplate") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:chainmail_helmet") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:chainmail_leggings") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:chicken") => Some(CookingRecipe { result: "minecraft:cooked_chicken", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:chorus_fruit") => Some(CookingRecipe { result: "minecraft:popped_chorus_fruit", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:clay") => Some(CookingRecipe { result: "minecraft:terracotta", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:clay_ball") => Some(CookingRecipe { result: "minecraft:brick", count: 1, experience: 0.3, cooking_time: 200 }),
        ("Smelting", "minecraft:coal_ore") => Some(CookingRecipe { result: "minecraft:coal", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:cobbled_deepslate") => Some(CookingRecipe { result: "minecraft:deepslate", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:cobblestone") => Some(CookingRecipe { result: "minecraft:stone", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:cod") => Some(CookingRecipe { result: "minecraft:cooked_cod", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_axe") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_boots") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_chestplate") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_helmet") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_hoe") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_horse_armor") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_leggings") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_nautilus_armor") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_ore") => Some(CookingRecipe { result: "minecraft:copper_ingot", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_pickaxe") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_shovel") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_spear") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:copper_sword") => Some(CookingRecipe { result: "minecraft:copper_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:cyan_terracotta") => Some(CookingRecipe { result: "minecraft:cyan_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_bricks") => Some(CookingRecipe { result: "minecraft:cracked_deepslate_bricks", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_coal_ore") => Some(CookingRecipe { result: "minecraft:coal", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_copper_ore") => Some(CookingRecipe { result: "minecraft:copper_ingot", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_diamond_ore") => Some(CookingRecipe { result: "minecraft:diamond", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_emerald_ore") => Some(CookingRecipe { result: "minecraft:emerald", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_gold_ore") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_iron_ore") => Some(CookingRecipe { result: "minecraft:iron_ingot", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_lapis_ore") => Some(CookingRecipe { result: "minecraft:lapis_lazuli", count: 1, experience: 0.2, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_redstone_ore") => Some(CookingRecipe { result: "minecraft:redstone", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:deepslate_tiles") => Some(CookingRecipe { result: "minecraft:cracked_deepslate_tiles", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:diamond_ore") => Some(CookingRecipe { result: "minecraft:diamond", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:emerald_ore") => Some(CookingRecipe { result: "minecraft:emerald", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:gold_ore") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_axe") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_boots") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_chestplate") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_helmet") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_hoe") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_horse_armor") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_leggings") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_nautilus_armor") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_pickaxe") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_shovel") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_spear") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:golden_sword") => Some(CookingRecipe { result: "minecraft:gold_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:gray_terracotta") => Some(CookingRecipe { result: "minecraft:gray_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:green_terracotta") => Some(CookingRecipe { result: "minecraft:green_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_axe") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_boots") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_chestplate") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_helmet") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_hoe") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_horse_armor") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_leggings") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_nautilus_armor") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_ore") => Some(CookingRecipe { result: "minecraft:iron_ingot", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_pickaxe") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_shovel") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_spear") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:iron_sword") => Some(CookingRecipe { result: "minecraft:iron_nugget", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:kelp") => Some(CookingRecipe { result: "minecraft:dried_kelp", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:lapis_ore") => Some(CookingRecipe { result: "minecraft:lapis_lazuli", count: 1, experience: 0.2, cooking_time: 200 }),
        ("Smelting", "minecraft:light_blue_terracotta") => Some(CookingRecipe { result: "minecraft:light_blue_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:light_gray_terracotta") => Some(CookingRecipe { result: "minecraft:light_gray_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:lime_terracotta") => Some(CookingRecipe { result: "minecraft:lime_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:magenta_terracotta") => Some(CookingRecipe { result: "minecraft:magenta_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:mutton") => Some(CookingRecipe { result: "minecraft:cooked_mutton", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:nether_bricks") => Some(CookingRecipe { result: "minecraft:cracked_nether_bricks", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:nether_gold_ore") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:nether_quartz_ore") => Some(CookingRecipe { result: "minecraft:quartz", count: 1, experience: 0.2, cooking_time: 200 }),
        ("Smelting", "minecraft:netherrack") => Some(CookingRecipe { result: "minecraft:nether_brick", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:orange_terracotta") => Some(CookingRecipe { result: "minecraft:orange_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:pink_terracotta") => Some(CookingRecipe { result: "minecraft:pink_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:polished_blackstone_bricks") => Some(CookingRecipe { result: "minecraft:cracked_polished_blackstone_bricks", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:porkchop") => Some(CookingRecipe { result: "minecraft:cooked_porkchop", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:potato") => Some(CookingRecipe { result: "minecraft:baked_potato", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:purple_terracotta") => Some(CookingRecipe { result: "minecraft:purple_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:quartz_block") => Some(CookingRecipe { result: "minecraft:smooth_quartz", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:rabbit") => Some(CookingRecipe { result: "minecraft:cooked_rabbit", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:raw_copper") => Some(CookingRecipe { result: "minecraft:copper_ingot", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:raw_gold") => Some(CookingRecipe { result: "minecraft:gold_ingot", count: 1, experience: 1.0, cooking_time: 200 }),
        ("Smelting", "minecraft:raw_iron") => Some(CookingRecipe { result: "minecraft:iron_ingot", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:red_sandstone") => Some(CookingRecipe { result: "minecraft:smooth_red_sandstone", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:red_terracotta") => Some(CookingRecipe { result: "minecraft:red_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:redstone_ore") => Some(CookingRecipe { result: "minecraft:redstone", count: 1, experience: 0.7, cooking_time: 200 }),
        ("Smelting", "minecraft:resin_clump") => Some(CookingRecipe { result: "minecraft:resin_brick", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:salmon") => Some(CookingRecipe { result: "minecraft:cooked_salmon", count: 1, experience: 0.35, cooking_time: 200 }),
        ("Smelting", "minecraft:sandstone") => Some(CookingRecipe { result: "minecraft:smooth_sandstone", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:sea_pickle") => Some(CookingRecipe { result: "minecraft:lime_dye", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:stone") => Some(CookingRecipe { result: "minecraft:smooth_stone", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:stone_bricks") => Some(CookingRecipe { result: "minecraft:cracked_stone_bricks", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:wet_sponge") => Some(CookingRecipe { result: "minecraft:sponge", count: 1, experience: 0.15, cooking_time: 200 }),
        ("Smelting", "minecraft:white_terracotta") => Some(CookingRecipe { result: "minecraft:white_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smelting", "minecraft:yellow_terracotta") => Some(CookingRecipe { result: "minecraft:yellow_glazed_terracotta", count: 1, experience: 0.1, cooking_time: 200 }),
        ("Smoking", "minecraft:beef") => Some(CookingRecipe { result: "minecraft:cooked_beef", count: 1, experience: 0.35, cooking_time: 100 }),
        ("Smoking", "minecraft:chicken") => Some(CookingRecipe { result: "minecraft:cooked_chicken", count: 1, experience: 0.35, cooking_time: 100 }),
        ("Smoking", "minecraft:cod") => Some(CookingRecipe { result: "minecraft:cooked_cod", count: 1, experience: 0.35, cooking_time: 100 }),
        ("Smoking", "minecraft:kelp") => Some(CookingRecipe { result: "minecraft:dried_kelp", count: 1, experience: 0.1, cooking_time: 100 }),
        ("Smoking", "minecraft:mutton") => Some(CookingRecipe { result: "minecraft:cooked_mutton", count: 1, experience: 0.35, cooking_time: 100 }),
        ("Smoking", "minecraft:porkchop") => Some(CookingRecipe { result: "minecraft:cooked_porkchop", count: 1, experience: 0.35, cooking_time: 100 }),
        ("Smoking", "minecraft:potato") => Some(CookingRecipe { result: "minecraft:baked_potato", count: 1, experience: 0.35, cooking_time: 100 }),
        ("Smoking", "minecraft:rabbit") => Some(CookingRecipe { result: "minecraft:cooked_rabbit", count: 1, experience: 0.35, cooking_time: 100 }),
        ("Smoking", "minecraft:salmon") => Some(CookingRecipe { result: "minecraft:cooked_salmon", count: 1, experience: 0.35, cooking_time: 100 }),
        _ => None,
    }
}

/// The nine flammable overworld wood species that appear across the
/// planks/slabs/stairs/doors/etc. tags (`logs_that_burn.json` is the
/// authoritative list minus bamboo, which has no log form but does appear in
/// the other wood-product tags) — see the module doc comment.
const FLAMMABLE_WOOD_SPECIES: &[&str] = &[
    "oak",
    "spruce",
    "birch",
    "jungle",
    "acacia",
    "dark_oak",
    "pale_oak",
    "mangrove",
    "cherry",
];

/// Same as [`FLAMMABLE_WOOD_SPECIES`] plus bamboo, for the tags that include
/// bamboo's plank-derived products (planks/slabs/stairs/doors/trapdoors/
/// fences/fence_gates/pressure_plates/buttons/signs/hanging_signs/shelves —
/// verified directly against each tag file, see the module doc comment).
const FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO: &[&str] = &[
    "oak",
    "spruce",
    "birch",
    "jungle",
    "acacia",
    "dark_oak",
    "pale_oak",
    "mangrove",
    "cherry",
    "bamboo",
];

fn strip_species_suffix<'a>(item: &'a str, suffixes: &[&str], species: &[&str]) -> Option<&'a str> {
    let path = item.strip_prefix("minecraft:")?;
    for suffix in suffixes {
        if let Some(prefix) = path.strip_suffix(suffix) {
            let prefix = prefix.strip_prefix("stripped_").unwrap_or(prefix);
            if species.contains(&prefix) {
                return Some(prefix);
            }
        }
    }
    None
}

/// The base (furnace-speed) fuel burn duration for `item`, restated from
/// `FuelValues.vanillaBurnTimes` (`baseUnit = 200`) — see the module doc
/// comment. `0` for anything not a fuel (matching `Object2IntSortedMap`'s
/// implicit zero default for a missing key, `FuelValues.burnDuration`,
/// `:34-36`).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn base_burn_duration(item: &str) -> i32 {
    const BASE: i32 = 200;

    // Non-flammable nether wood is excluded outright, before any pattern
    // below could otherwise (incorrectly) match a `_planks`/`_slab`/...
    // suffix on it (`non_flammable_wood.json`: exactly `crimson_`/`warped_`
    // prefixed items).
    if let Some(path) = item.strip_prefix("minecraft:") {
        if path.starts_with("crimson_") || path.starts_with("warped_") {
            return 0;
        }
    }

    match item {
        "minecraft:lava_bucket" => return BASE * 100,
        "minecraft:coal_block" => return BASE * 8 * 10,
        "minecraft:blaze_rod" => return BASE * 12,
        "minecraft:coal" | "minecraft:charcoal" => return BASE * 8,
        "minecraft:bamboo_mosaic" | "minecraft:bamboo_mosaic_stairs" => return BASE * 3 / 2,
        "minecraft:bamboo_mosaic_slab" => return BASE * 3 / 4,
        "minecraft:note_block"
        | "minecraft:bookshelf"
        | "minecraft:chiseled_bookshelf"
        | "minecraft:lectern"
        | "minecraft:jukebox"
        | "minecraft:chest"
        | "minecraft:trapped_chest"
        | "minecraft:crafting_table"
        | "minecraft:daylight_detector"
        | "minecraft:bow"
        | "minecraft:fishing_rod"
        | "minecraft:ladder"
        | "minecraft:crossbow"
        | "minecraft:loom"
        | "minecraft:barrel"
        | "minecraft:cartography_table"
        | "minecraft:fletching_table"
        | "minecraft:smithing_table"
        | "minecraft:composter"
        | "minecraft:mangrove_roots" => return BASE * 3 / 2,
        "minecraft:wooden_shovel"
        | "minecraft:wooden_sword"
        | "minecraft:wooden_spear"
        | "minecraft:wooden_hoe"
        | "minecraft:wooden_axe"
        | "minecraft:wooden_pickaxe" => return BASE,
        "minecraft:stick" => return BASE / 2,
        "minecraft:bowl" => return BASE / 2,
        "minecraft:dried_kelp_block" => return 1 + BASE * 20,
        "minecraft:bamboo" => return BASE / 4,
        "minecraft:dead_bush" | "minecraft:short_dry_grass" | "minecraft:tall_dry_grass" => {
            return BASE / 2;
        }
        "minecraft:scaffolding" => return BASE / 4,
        "minecraft:azalea" | "minecraft:flowering_azalea" => return BASE / 2,
        "minecraft:leaf_litter" => return BASE / 2,
        _ => {}
    }

    if item == "minecraft:bamboo_block" || item == "minecraft:stripped_bamboo_block" {
        return BASE * 3 / 2;
    }

    if let Some(path) = item.strip_prefix("minecraft:") {
        if let Some(rest) = path.strip_suffix("_wool") {
            let _ = rest;
            return BASE / 2;
        }
        if let Some(rest) = path.strip_suffix("_carpet") {
            let _ = rest;
            return 1 + BASE / 3;
        }
        if let Some(rest) = path.strip_suffix("_banner") {
            let _ = rest;
            return BASE * 3 / 2;
        }
    }

    if strip_species_suffix(item, &["_log", "_wood"], FLAMMABLE_WOOD_SPECIES).is_some() {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_sapling"], FLAMMABLE_WOOD_SPECIES).is_some() {
        return BASE / 2;
    }
    if strip_species_suffix(item, &["_planks"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_stairs"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_slab"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE * 3 / 4;
    }
    if strip_species_suffix(item, &["_trapdoor"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_pressure_plate"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO)
        .is_some()
    {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_fence_gate"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_fence"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_shelf"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE * 3 / 2;
    }
    if strip_species_suffix(item, &["_hanging_sign"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some()
    {
        return BASE * 4;
    }
    // `_sign` must be checked after `_hanging_sign` for the same reason as
    // `_fence`/`_fence_gate` above.
    if strip_species_suffix(item, &["_sign"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE;
    }
    if strip_species_suffix(item, &["_door"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE;
    }
    if strip_species_suffix(item, &["_button"], FLAMMABLE_WOOD_SPECIES_WITH_BAMBOO).is_some() {
        return BASE / 2;
    }
    // Boats are per-species items (`oak_boat`, ..., `ItemTags.BOATS`,
    // `FuelValues.java:84`); bamboo's boat is irregularly named
    // `bamboo_raft` (`boats.json`), handled as its own concrete entry rather
    // than the `_boat` suffix. Chest boats (`#minecraft:chest_boats`,
    // nested inside `boats.json`) are not modeled — a narrower, documented
    // gap (chest boats are a rare fuel source).
    if item == "minecraft:bamboo_raft" {
        return BASE * 6;
    }
    if strip_species_suffix(item, &["_boat"], FLAMMABLE_WOOD_SPECIES).is_some() {
        return BASE * 6;
    }

    0
}

/// The halving strategy per furnace kind: [`FurnaceKind::Furnace`] burns
/// fuel at the base rate, [`FurnaceKind::Smoker`]/[`FurnaceKind::BlastFurnace`]
/// halve it (`SmokerBlockEntity`/`BlastFurnaceBlockEntity.getBurnDuration`:
/// `return super.getBurnDuration(fuelValues, itemStack) / 2;`) — Java integer
/// division, matched here with `i32` division.
#[must_use]
pub fn effective_burn_duration(kind: FurnaceKind, item: &str) -> i32 {
    let base = base_burn_duration(item);
    match kind {
        FurnaceKind::Furnace => base,
        FurnaceKind::Smoker | FurnaceKind::BlastFurnace => base / 2,
    }
}

/// What changed on one [`Furnace::tick`] call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FurnaceTick {
    /// `Some(now_lit)` when the lit/unlit block-state boolean flipped this
    /// tick (`AbstractFurnaceBlock.LIT`) — the caller's cue to sync a block
    /// update.
    pub lit_changed: Option<bool>,
    /// Whether a cook completed and moved into the output slot this tick.
    pub cooked: bool,
    /// Whether one fuel item was consumed to relight the fire this tick.
    pub fuel_consumed: bool,
}

/// A furnace-family block entity's burn/cook state, shared by the furnace,
/// smoker, and blast furnace (see the module doc comment for the vanilla
/// citation `AbstractFurnaceBlockEntity` this ports).
#[derive(Debug, Clone, PartialEq)]
pub struct Furnace {
    kind: FurnaceKind,
    input: Option<ItemStack>,
    fuel: Option<ItemStack>,
    output: Option<ItemStack>,
    lit_time_remaining: i32,
    lit_total_time: i32,
    cooking_timer: i32,
    cooking_total_time: i32,
    /// Recipe key (`"kind:ingredient"`) -> times cooked since the last
    /// [`take_recipes_used`](Self::take_recipes_used) drain — vanilla's
    /// `recipesUsed` (`Reference2IntOpenHashMap`), banked but not paid out
    /// until collection (see the module doc comment and
    /// [`experience_for`]).
    recipes_used: HashMap<String, u32>,
}

impl Furnace {
    /// A freshly placed, empty, unlit furnace of the given kind.
    #[must_use]
    pub fn new(kind: FurnaceKind) -> Self {
        Self {
            kind,
            input: None,
            fuel: None,
            output: None,
            lit_time_remaining: 0,
            lit_total_time: 0,
            cooking_timer: 0,
            cooking_total_time: 0,
            recipes_used: HashMap::new(),
        }
    }

    /// Rebuilds a furnace from persisted state — every field at once,
    /// deliberately, rather than a family of setters.
    ///
    /// The totality is the point: this is the only constructor world loading
    /// uses, so adding a field to [`Furnace`] breaks it at compile time and
    /// forces the save schema to be updated with it. A setter-based restore
    /// would silently drop the new field and a world would come back subtly
    /// wrong with every test still green.
    ///
    /// `recipes_used` is the banked, not-yet-collected experience map (see
    /// [`take_recipes_used`](Self::take_recipes_used)); its keys are this
    /// crate's own `"kind:ingredient"` strings, not vanilla recipe ids, which
    /// is why [`crate::chunk_nbt`] writes it under a namespaced field of its
    /// own rather than into vanilla's `RecipesUsed`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn restore(
        kind: FurnaceKind,
        input: Option<ItemStack>,
        fuel: Option<ItemStack>,
        output: Option<ItemStack>,
        lit_time_remaining: i32,
        lit_total_time: i32,
        cooking_time_spent: i32,
        cooking_total_time: i32,
        recipes_used: HashMap<String, u32>,
    ) -> Self {
        Self {
            kind,
            input,
            fuel,
            output,
            lit_time_remaining,
            lit_total_time,
            cooking_timer: cooking_time_spent,
            cooking_total_time,
            recipes_used,
        }
    }

    /// The four burn/cook timers in vanilla's own `AbstractFurnaceBlockEntity`
    /// field order: `lit_time_remaining`, `lit_total_time`,
    /// `cooking_time_spent`, `cooking_total_time`.
    ///
    /// Distinct from [`container_data`](Self::container_data), which answers
    /// the same four numbers *in menu-property order* for the wire. This one
    /// is named after the on-disk fields so a save site cannot pair a value
    /// with the wrong key by reading the indices in the wrong order.
    #[must_use]
    pub fn burn_state(&self) -> (i32, i32, i32, i32) {
        (
            self.lit_time_remaining,
            self.lit_total_time,
            self.cooking_timer,
            self.cooking_total_time,
        )
    }

    /// The banked recipe-use counts, for persistence. Read-only: collection
    /// still goes through [`take_recipes_used`](Self::take_recipes_used),
    /// which is what actually pays the experience out.
    #[must_use]
    pub fn recipes_used(&self) -> &HashMap<String, u32> {
        &self.recipes_used
    }

    #[must_use]
    pub fn kind(&self) -> FurnaceKind {
        self.kind
    }

    #[must_use]
    pub fn input(&self) -> Option<&ItemStack> {
        self.input.as_ref()
    }

    #[must_use]
    pub fn fuel(&self) -> Option<&ItemStack> {
        self.fuel.as_ref()
    }

    #[must_use]
    pub fn output(&self) -> Option<&ItemStack> {
        self.output.as_ref()
    }

    #[must_use]
    pub fn is_lit(&self) -> bool {
        self.lit_time_remaining > 0
    }

    #[must_use]
    pub fn cooking_progress(&self) -> (i32, i32) {
        (self.cooking_timer, self.cooking_total_time)
    }

    /// The four values vanilla's `ContainerData` exposes for this block
    /// entity's menu (`AbstractFurnaceBlockEntity.DATA_LIT_TIME` = 0,
    /// `DATA_LIT_DURATION` = 1, `DATA_COOKING_PROGRESS` = 2,
    /// `DATA_COOKING_TOTAL_TIME` = 3, `:46-53`) — exposed here so a future
    /// wiring layer can feed `ServerProtocol::encode_container_set_data`
    /// (not implemented anywhere yet; see this crate's top-level report for
    /// the declared gap) without re-deriving the index mapping.
    #[must_use]
    pub fn container_data(&self, index: u8) -> i32 {
        match index {
            0 => self.lit_time_remaining,
            1 => self.lit_total_time,
            2 => self.cooking_timer,
            3 => self.cooking_total_time,
            _ => 0,
        }
    }

    /// Sets the input (slot 0), mirroring `setItem`'s slot-0 branch
    /// (`:291-302`): when the new item differs from what was there
    /// (`!ItemStack.isSameItemSameComponents`), immediately recomputes
    /// `cookingTotalTime` from the new recipe (or [`DEFAULT_COOK_TIME`] if
    /// none) and resets `cookingTimer` to `0` — this is *not* deferred to
    /// the next tick.
    pub fn set_input(&mut self, item: Option<ItemStack>) {
        let same = matches!((&item, &self.input), (Some(a), Some(b)) if a.item == b.item && a.components == b.components);
        self.input = item;
        if !same {
            self.cooking_total_time = self
                .recipe_for_input()
                .map_or(DEFAULT_COOK_TIME, |r| r.cooking_time);
            self.cooking_timer = 0;
        }
    }

    /// Sets the fuel (slot 1). Vanilla's `setItem` has no special-case
    /// branch for this slot (only slot 0 triggers a recompute), so this is a
    /// plain write.
    pub fn set_fuel(&mut self, item: Option<ItemStack>) {
        self.fuel = item;
    }

    /// Writes the output slot (slot 2) directly, with no recipe-driven side
    /// effect (unlike [`set_input`](Self::set_input), vanilla's `setItem`
    /// slot-2 branch does nothing but the assignment — `:291-302` only
    /// special-cases slot 0). This is the counterpart a `container_click`
    /// consumer needs to apply the client's own predicted diff verbatim (see
    /// `docs/server-inventory.md`'s "applies the client's diff directly"
    /// scope note) when that diff clears or shrinks the result slot — the
    /// normal way a real client empties a cooked item out of a furnace.
    pub fn set_output(&mut self, item: Option<ItemStack>) {
        self.output = item;
    }

    /// Takes up to `count` items from the output slot (slot 2), shrinking or
    /// clearing it. Does **not** bank experience — vanilla pays that out
    /// separately, on menu close (`awardUsedRecipesAndPopExperience`); see
    /// [`take_recipes_used`](Self::take_recipes_used).
    pub fn take_output(&mut self, count: u32) -> Option<ItemStack> {
        let stack = self.output.as_mut()?;
        let take = count.min(stack.count);
        if take == 0 {
            return None;
        }
        let mut taken = stack.clone();
        taken.count = take;
        stack.count -= take;
        if stack.count == 0 {
            self.output = None;
        }
        Some(taken)
    }

    /// Drains the banked recipe-use counts (recipe key -> times cooked since
    /// the last drain), matching `awardUsedRecipesAndPopExperience`'s
    /// `this.recipesUsed.clear()` (`:343`). A caller wires this to whatever
    /// stands in for "the player closed the furnace menu" — not done by
    /// this crate today (see the module doc comment / the top-level report
    /// for the declared gap); pair with [`experience_for`] to turn a drained
    /// `(recipe_key, count)` into an XP amount.
    pub fn take_recipes_used(&mut self) -> HashMap<String, u32> {
        std::mem::take(&mut self.recipes_used)
    }

    fn recipe_for_input(&self) -> Option<CookingRecipe> {
        let input = self.input.as_ref()?;
        recipe_for(self.kind, &input.item.to_string())
    }

    fn effective_burn_duration_for(&self, item: &str) -> i32 {
        effective_burn_duration(self.kind, item)
    }

    /// `canBurn` (`:218-231`): the output slot must be empty, or already
    /// hold the exact same item (ignoring count) with room under
    /// [`MAX_STACK_SIZE`] for the new output.
    fn can_burn(&self, recipe: &CookingRecipe) -> bool {
        let Some(result_id): Option<lodestone_model::ResourceKey> = recipe.result.parse().ok()
        else {
            return false;
        };
        match &self.output {
            None => true,
            Some(out) => {
                if out.item != result_id {
                    return false;
                }
                out.count + recipe.count <= MAX_STACK_SIZE
            }
        }
    }

    fn consume_fuel(&mut self) {
        if let Some(stack) = self.fuel.as_mut() {
            stack.count = stack.count.saturating_sub(1);
            if stack.count == 0 {
                self.fuel = None;
            }
        }
    }

    /// `burn` (`:233-246`), minus the wet-sponge special case (see the
    /// module doc comment).
    fn burn(&mut self, recipe: &CookingRecipe, ingredient_key: &str) {
        let Ok(result_id) = recipe.result.parse::<lodestone_model::ResourceKey>() else {
            return;
        };
        match self.output.as_mut() {
            Some(out) if out.item == result_id => out.count += recipe.count,
            _ => self.output = Some(ItemStack::new(result_id, recipe.count)),
        }

        if let Some(inp) = self.input.as_mut() {
            inp.count = inp.count.saturating_sub(1);
            if inp.count == 0 {
                self.input = None;
            }
        }

        *self
            .recipes_used
            .entry(format!("{}:{ingredient_key}", self.kind.recipe_table_key()))
            .or_insert(0) += 1;
    }

    /// Advances the furnace by exactly one server tick — a direct port of
    /// `AbstractFurnaceBlockEntity.serverTick` (`:139-207`); see the module
    /// doc comment for the quoted control flow this mirrors line-by-line.
    pub fn tick(&mut self) -> FurnaceTick {
        let mut out = FurnaceTick::default();

        let was_lit = self.lit_time_remaining > 0;
        let mut is_lit = false;
        if was_lit {
            self.lit_time_remaining -= 1;
            is_lit = self.lit_time_remaining > 0;
        }

        let has_ingredient = self.input.is_some();
        let has_fuel = self.fuel.is_some();

        if is_lit || (has_fuel && has_ingredient) {
            if has_ingredient {
                // Borrow the ingredient id up front — `burn` below needs a
                // mutable borrow of `self`, so nothing here may keep holding
                // a `&self.input` reference across it.
                let ingredient_key = self.input.as_ref().unwrap().item.to_string();
                if let Some(recipe) = self.recipe_for_input() {
                    if self.can_burn(&recipe) {
                        if !is_lit {
                            let fuel_key = self.fuel.as_ref().map(|f| f.item.to_string());
                            let new_lit_time = fuel_key
                                .as_deref()
                                .map_or(0, |k| self.effective_burn_duration_for(k));
                            self.lit_time_remaining = new_lit_time;
                            self.lit_total_time = new_lit_time;
                            if new_lit_time > 0 {
                                self.consume_fuel();
                                is_lit = true;
                                out.fuel_consumed = true;
                            }
                        }

                        if is_lit {
                            self.cooking_timer += 1;
                            if self.cooking_timer == self.cooking_total_time {
                                self.cooking_timer = 0;
                                self.cooking_total_time = recipe.cooking_time;
                                self.burn(&recipe, &ingredient_key);
                                out.cooked = true;
                            }
                        } else {
                            self.cooking_timer = 0;
                        }
                    } else {
                        self.cooking_timer = 0;
                    }
                }
                // `recipe_for_input()` returning `None` leaves
                // `cooking_timer` untouched here — this matches vanilla's
                // own control flow exactly (`serverTick`'s `if (recipe !=
                // null) { ... }` has no `else`), not an oversight.
            } else {
                self.cooking_timer = 0;
            }
        } else if self.cooking_timer > 0 {
            self.cooking_timer = (self.cooking_timer - BURN_COOL_SPEED).clamp(0, self.cooking_total_time);
        }

        if was_lit != is_lit {
            out.lit_changed = Some(is_lit);
        }
        out
    }
}

/// `AbstractFurnaceBlockEntity.createExperience` (`:361-369`): `amount`
/// successful cooks of a recipe worth `experience_per_item` XP each bank
/// `floor(amount * experience_per_item)` XP for certain, plus one more with
/// probability equal to the fractional remainder — `roll` is the injected
/// `[0.0, 1.0)` sample standing in for `level.getRandom().nextFloat()`, the
/// same "caller supplies the randomness" shape [`crate::composter::Composter::insert`]
/// uses.
#[must_use]
/// Turns one drained [`Furnace::take_recipes_used`] map into a total XP award —
/// vanilla's `AbstractFurnaceBlockEntity.awardUsedRecipesAndPopExperience`, which
/// walks the banked recipes and calls `createExperience` for each.
///
/// `roll` is one `[0.0, 1.0)` draw **per recipe entry**, supplied by the caller in
/// the order the map is walked, because `createExperience`'s fractional remainder is
/// probabilistic (see [`experience_for`]). The map is a `HashMap`, so its iteration
/// order is not reproducible — which matters only for *which* entry gets which roll,
/// not for the total's distribution, and is recorded rather than papered over.
///
/// A recipe key the table no longer knows contributes nothing rather than panicking:
/// a furnace loaded from a save written by a different build can carry one.
#[must_use]
pub fn experience_for_recipes(
    used: &std::collections::HashMap<String, u32>,
    mut rolls: impl FnMut() -> f32,
) -> u32 {
    let mut total = 0u32;
    for (key, count) in used {
        // The key is `"<table>:<ingredient>"` — `Furnace::tick`'s own format. Split
        // on the *first* colon only: the ingredient is itself a namespaced id and
        // carries a second one.
        let Some((table, ingredient)) = key.split_once(':') else {
            continue;
        };
        let Some(recipe) = cooking_recipe(table, ingredient) else {
            continue;
        };
        total = total.saturating_add(experience_for(*count, recipe.experience, rolls()));
    }
    total
}

pub fn experience_for(amount: u32, experience_per_item: f32, roll: f32) -> u32 {
    let raw = amount as f32 * experience_per_item;
    let mut xp = raw.floor();
    let frac = raw - xp;
    if frac != 0.0 && roll < frac {
        xp += 1.0;
    }
    xp as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid resource key"), count)
    }

    #[test]
    fn fresh_furnace_is_unlit_and_empty() {
        let f = Furnace::new(FurnaceKind::Furnace);
        assert!(!f.is_lit());
        assert_eq!(f.cooking_progress(), (0, 0));
        assert!(f.input().is_none());
        assert!(f.output().is_none());
    }

    #[test]
    fn base_burn_durations_match_fuel_values_java() {
        // FuelValues.java:44-107, baseUnit = 200.
        assert_eq!(base_burn_duration("minecraft:lava_bucket"), 20_000);
        assert_eq!(base_burn_duration("minecraft:coal_block"), 16_000);
        assert_eq!(base_burn_duration("minecraft:blaze_rod"), 2_400);
        assert_eq!(base_burn_duration("minecraft:coal"), 1_600);
        assert_eq!(base_burn_duration("minecraft:charcoal"), 1_600);
        assert_eq!(base_burn_duration("minecraft:oak_planks"), 300);
        assert_eq!(base_burn_duration("minecraft:oak_slab"), 150);
        assert_eq!(base_burn_duration("minecraft:stick"), 100);
        assert_eq!(base_burn_duration("minecraft:oak_sapling"), 100);
        assert_eq!(base_burn_duration("minecraft:white_wool"), 100);
        assert_eq!(base_burn_duration("minecraft:white_carpet"), 67);
        assert_eq!(base_burn_duration("minecraft:dried_kelp_block"), 4_001);
        assert_eq!(base_burn_duration("minecraft:bamboo"), 50);
        assert_eq!(base_burn_duration("minecraft:bamboo_planks"), 300);
        assert_eq!(base_burn_duration("minecraft:bamboo_raft"), 1_200);
        assert_eq!(base_burn_duration("minecraft:oak_boat"), 1_200);
        assert_eq!(base_burn_duration("minecraft:oak_hanging_sign"), 800);
        assert_eq!(base_burn_duration("minecraft:oak_sign"), 200);
        assert_eq!(base_burn_duration("minecraft:stripped_oak_log"), 300);
    }

    /// **Control**: crimson/warped items must be excluded even though they
    /// would otherwise match a `_planks`/`_log`/... suffix rule exactly like
    /// their overworld counterparts — proves the exclusion is real, not
    /// merely that these particular strings coincidentally never matched
    /// any rule.
    #[test]
    fn non_flammable_nether_wood_has_zero_burn_duration() {
        assert_eq!(base_burn_duration("minecraft:crimson_planks"), 0);
        assert_eq!(base_burn_duration("minecraft:warped_stairs"), 0);
        assert_eq!(base_burn_duration("minecraft:crimson_hanging_sign"), 0);
        // Non-fuel items are also zero, same code path.
        assert_eq!(base_burn_duration("minecraft:diamond"), 0);
    }

    #[test]
    fn smoker_and_blast_furnace_halve_fuel_duration() {
        assert_eq!(effective_burn_duration(FurnaceKind::Furnace, "minecraft:coal"), 1_600);
        assert_eq!(effective_burn_duration(FurnaceKind::Smoker, "minecraft:coal"), 800);
        assert_eq!(
            effective_burn_duration(FurnaceKind::BlastFurnace, "minecraft:coal"),
            800
        );
    }

    #[test]
    fn recipe_lookup_is_scoped_to_the_right_furnace_kind() {
        // Iron ore smelts in both furnace and blast furnace, at different
        // speeds (2x in the blast furnace).
        let smelting = recipe_for(FurnaceKind::Furnace, "minecraft:iron_ore").unwrap();
        assert_eq!(smelting.result, "minecraft:iron_ingot");
        assert_eq!(smelting.cooking_time, 200);
        assert_eq!(smelting.experience, 0.7);

        let blasting = recipe_for(FurnaceKind::BlastFurnace, "minecraft:iron_ore").unwrap();
        assert_eq!(blasting.result, "minecraft:iron_ingot");
        assert_eq!(blasting.cooking_time, 100);

        // Every vanilla `minecraft:smoking` food recipe in this data set
        // also has a `minecraft:smelting` counterpart (just slower — 200 vs
        // 100 ticks), so chicken *does* resolve under `Furnace` too. The
        // real kind-exclusivity in this table is ores/tools: `Blasting`
        // recipes (gold nugget recycling, `iron_nugget_from_blasting.json`)
        // do not resolve under `Smoking`, and a `Smelting`-only block
        // recipe (plain cobblestone -> stone, no faster blasting variant
        // exists for it) does not resolve under `Blasting` either.
        let smoking = recipe_for(FurnaceKind::Smoker, "minecraft:chicken").unwrap();
        assert_eq!(smoking.result, "minecraft:cooked_chicken");
        assert_eq!(smoking.cooking_time, 100);
        assert!(recipe_for(FurnaceKind::Smoker, "minecraft:golden_pickaxe").is_none());
        assert!(recipe_for(FurnaceKind::Furnace, "minecraft:cobblestone").is_some());
        assert!(recipe_for(FurnaceKind::BlastFurnace, "minecraft:cobblestone").is_none());
        assert!(recipe_for(FurnaceKind::Smoker, "minecraft:cobblestone").is_none());
    }

    /// The magnitude check this repo's rules require: not just "an ingot
    /// eventually comes out", but the *exact* tick, with coal (1600 ticks of
    /// fuel, far more than the 200 needed) driving a 200-tick smelt.
    #[test]
    fn iron_ore_smelts_into_one_ingot_at_exactly_tick_200() {
        let mut f = Furnace::new(FurnaceKind::Furnace);
        f.set_input(Some(stack("minecraft:iron_ore", 1)));
        f.set_fuel(Some(stack("minecraft:coal", 1)));
        assert_eq!(f.cooking_progress(), (0, 200));

        for t in 1..200 {
            let tick = f.tick();
            assert!(!tick.cooked, "cooked early at tick {t}");
        }
        let tick = f.tick();
        assert!(tick.cooked, "expected the cook to complete at tick 200");
        assert_eq!(f.output(), Some(&stack("minecraft:iron_ingot", 1)));
        assert_eq!(f.input(), None, "the single iron ore must be fully consumed");
        assert_eq!(f.cooking_progress(), (0, 200), "timer resets after a completed cook");
    }

    /// The blast furnace halves cook time (100 ticks) *and* fuel duration —
    /// two independent halvings that must not be conflated. Coal still
    /// lasts 800 ticks here (`effective_burn_duration`), far more than the
    /// 100 needed, so only the cook-time halving is actually exercised by
    /// the completion tick.
    #[test]
    fn blast_furnace_smelts_in_half_the_ticks() {
        let mut f = Furnace::new(FurnaceKind::BlastFurnace);
        f.set_input(Some(stack("minecraft:iron_ore", 1)));
        f.set_fuel(Some(stack("minecraft:coal", 1)));
        assert_eq!(f.cooking_progress(), (0, 100));

        for _ in 1..100 {
            assert!(!f.tick().cooked);
        }
        assert!(f.tick().cooked);
        assert_eq!(f.output(), Some(&stack("minecraft:iron_ingot", 1)));
    }

    /// Fuel lights on the very first tick that has both fuel and an
    /// ingredient, consuming exactly one fuel item and flipping
    /// `lit_changed` from unlit to lit.
    #[test]
    fn fuel_lights_on_first_tick_and_is_consumed_once() {
        let mut f = Furnace::new(FurnaceKind::Furnace);
        f.set_input(Some(stack("minecraft:iron_ore", 1)));
        f.set_fuel(Some(stack("minecraft:coal", 1)));

        let tick = f.tick();
        assert_eq!(tick.lit_changed, Some(true));
        assert!(tick.fuel_consumed);
        assert!(f.is_lit());
        assert!(f.fuel().is_none(), "the single coal must be fully consumed");
    }

    /// **Control**: with no fuel at all, an ingredient alone must never
    /// light the furnace or advance cooking progress, no matter how many
    /// ticks pass.
    #[test]
    fn no_fuel_means_no_cooking_ever() {
        let mut f = Furnace::new(FurnaceKind::Furnace);
        f.set_input(Some(stack("minecraft:iron_ore", 1)));
        for t in 0..500 {
            let tick = f.tick();
            assert!(!tick.cooked, "cooked with no fuel at tick {t}");
            assert!(!f.is_lit(), "lit with no fuel at tick {t}");
        }
        assert_eq!(f.cooking_progress().0, 0);
    }

    /// **Control**: a blocked output (already holding a different, unrelated
    /// item) must prevent the cook from ever completing, even with ample
    /// fuel and a valid ingredient — `can_burn` is really gating, not
    /// coincidentally always true in the other tests.
    #[test]
    fn full_output_of_a_different_item_blocks_cooking_forever() {
        let mut f = Furnace::new(FurnaceKind::Furnace);
        f.set_input(Some(stack("minecraft:iron_ore", 1)));
        f.set_fuel(Some(stack("minecraft:coal", 1)));
        // Pre-seed the output slot with an unrelated item the way a stuck
        // furnace with the wrong recipe cooked earlier might.
        f.output = Some(stack("minecraft:gold_ingot", 1));

        for t in 0..300 {
            let tick = f.tick();
            assert!(!tick.cooked, "cooked into a blocked output at tick {t}");
        }
        assert_eq!(f.output(), Some(&stack("minecraft:gold_ingot", 1)));
        assert_eq!(
            f.input(),
            Some(&stack("minecraft:iron_ore", 1)),
            "ingredient must not be consumed while blocked"
        );
    }

    /// Cooking progress decays (not resets) once the fire goes out with
    /// nothing left to relight it — `BURN_COOL_SPEED = 2` per tick, clamped
    /// at 0, never past `cooking_total_time`.
    #[test]
    fn progress_decays_by_two_per_tick_once_fuel_runs_out() {
        let mut f = Furnace::new(FurnaceKind::Furnace);
        // A single stick (100 ticks of fuel) is not enough to finish a
        // 200-tick smelt, so the fire goes out mid-cook. Lighting happens on
        // tick 1 (setting `lit_time_remaining` to 100 directly, with no
        // decrement that same tick — see `Furnace::tick`), so the fire is
        // still lit after ticks 1..=100 (100 ticks of cooking progress
        // banked) and only actually goes out on tick 101, the first tick
        // whose *decrement* drives `lit_time_remaining` to 0.
        f.set_input(Some(stack("minecraft:iron_ore", 1)));
        f.set_fuel(Some(stack("minecraft:stick", 1)));

        for t in 1..=100 {
            let tick = f.tick();
            assert!(f.is_lit(), "expected still lit at tick {t}");
            assert!(!tick.cooked);
        }
        assert_eq!(f.cooking_progress().0, 100, "100 ticks of progress banked while lit");

        // Tick 101: the fire goes out (no fuel left to relight it) and the
        // cooling decay fires in the very same tick, matching vanilla's
        // single-pass `if (isLit || ...) {...} else if (cookingTimer > 0)
        // {...}` control flow.
        let tick = f.tick();
        assert_eq!(tick.lit_changed, Some(false), "expected the unlit flip at tick 101");
        assert!(!f.is_lit());
        assert_eq!(f.cooking_progress().0, 98, "100 - BURN_COOL_SPEED (2) in the same tick");

        // Further ticks keep decaying by exactly 2, with no more lit_changed
        // flips (already unlit).
        let tick = f.tick();
        assert_eq!(tick.lit_changed, None);
        assert_eq!(f.cooking_progress().0, 96);
    }

    #[test]
    fn take_output_shrinks_and_clears_the_slot() {
        let mut f = Furnace::new(FurnaceKind::Furnace);
        f.output = Some(stack("minecraft:iron_ingot", 3));
        assert_eq!(f.take_output(2), Some(stack("minecraft:iron_ingot", 2)));
        assert_eq!(f.output(), Some(&stack("minecraft:iron_ingot", 1)));
        assert_eq!(f.take_output(5), Some(stack("minecraft:iron_ingot", 1)));
        assert_eq!(f.output(), None);
        assert_eq!(f.take_output(1), None);
    }

    #[test]
    fn recipes_used_bank_per_cook_and_drain_clears_them() {
        let mut f = Furnace::new(FurnaceKind::Furnace);
        f.set_input(Some(stack("minecraft:iron_ore", 2)));
        f.set_fuel(Some(stack("minecraft:coal", 2)));

        for _ in 0..200 {
            f.tick();
        }
        assert_eq!(f.output(), Some(&stack("minecraft:iron_ingot", 1)));

        // Re-seed a second ore now that the first finished, using the
        // second coal already in the fuel slot from the initial fill.
        f.set_input(Some(stack("minecraft:iron_ore", 1)));
        for _ in 0..200 {
            f.tick();
        }
        assert_eq!(f.output(), Some(&stack("minecraft:iron_ingot", 2)));

        let used = f.take_recipes_used();
        assert_eq!(used.get("Smelting:minecraft:iron_ore"), Some(&2));
        assert!(
            f.take_recipes_used().is_empty(),
            "a second drain with nothing cooked since must be empty"
        );
    }

    #[test]
    fn experience_for_matches_create_experience_floor_and_probabilistic_remainder() {
        // 3 cooks at 0.7 xp each = 2.1 -> floor 2, +1 with probability 0.1.
        assert_eq!(experience_for(3, 0.7, 0.05), 3, "roll below the 0.1 fraction rounds up");
        assert_eq!(experience_for(3, 0.7, 0.5), 2, "roll above the fraction does not round up");
        // An exact integer amount has zero fractional remainder — the roll
        // must never matter.
        assert_eq!(experience_for(2, 1.0, 0.999), 2);
    }
    /// `experience_for_recipes` turns a banked map into a total — the join that made
    /// `take_recipes_used` and `experience_for` reachable at all.
    ///
    /// The key format is `"<table>:<ingredient>"` and the ingredient is itself a
    /// namespaced id, so **the split must be on the first colon only**. Splitting on
    /// the last (or on every) colon yields a table name no recipe lookup knows, and
    /// the function would silently return zero for every entry — a plausible "no XP
    /// yet" rather than a failure.
    ///
    /// Iron ore smelts for `0.7` XP each in 26.2. Ten cooks is `7.0` exactly, so the
    /// fractional remainder is zero and the roll cannot matter: the total is exactly
    /// **7**, whatever the RNG does. That is the prediction, and it is roll-free by
    /// construction rather than by tolerance.
    #[test]
    fn experience_for_recipes_splits_the_key_on_the_first_colon_only() {
        let recipe = recipe_for(FurnaceKind::Furnace, "minecraft:raw_iron")
            .expect("raw iron is smeltable in 26.2");
        // The expected value comes from the recipe table, not from a literal here.
        let per_item = recipe.experience;
        let cooks = 10u32;
        let exact = (cooks as f32 * per_item).fract() == 0.0;

        let mut used = std::collections::HashMap::new();
        used.insert(
            format!("{}:minecraft:raw_iron", FurnaceKind::Furnace.recipe_table_key()),
            cooks,
        );

        let low = experience_for_recipes(&used, || 0.0);
        let high = experience_for_recipes(&used, || 0.999);
        assert_eq!(
            low,
            (cooks as f32 * per_item) as u32,
            "the total must be the table's own per-item value times the cook count"
        );
        if exact {
            assert_eq!(
                low, high,
                "with a whole-number total the roll cannot matter, so both extremes \
                 must agree"
            );
        }
        assert!(low > 0, "a banked smelt must be worth something");

        // **The control**: a key the table does not know contributes nothing, rather
        // than panicking or being counted at some default. This is also what a
        // wrong split would make *every* key look like, so the two assertions
        // together separate "unknown recipe" from "broken parser".
        let mut bogus = std::collections::HashMap::new();
        bogus.insert("minecraft:smelting:minecraft:not_a_thing".to_string(), 99);
        assert_eq!(experience_for_recipes(&bogus, || 0.0), 0);

        // And an empty map is zero, not a panic.
        assert_eq!(
            experience_for_recipes(&std::collections::HashMap::new(), || 0.0),
            0
        );
    }
}
