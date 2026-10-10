# Crafting

## What it is

The version-free crafting stack in `lodestone-game`: the recipe data model and matching rules (`recipe.rs`), a loader for the datapack's recipe JSON (`recipe_json.rs`), the crafting-table menu layout, the plugin-facing recipe-registration API, and the recipe-book UI (browsing, auto-fill, unlock toast). Our server also computes crafting results; see [Server-authoritative gameplay](./server-gameplay.md).

## How it works

### Who computes the result slot

**The server does**: it runs the crafting-menu slot-change hook and pushes the result as `container_set_slot` for slot 0, and the reference client never matches recipes itself. So a local corpus is not on the critical path for "put items in, see the result"; it exists for the recipe-book UI, ghost previews before the round trip, and offline use. Since 1.21.2 `update_recipes` carries only recipe property sets and the stonecutter list, and the recipe book arrives separately via `recipe_book_add` as display-only entries; neither substitutes for the datapack corpus.

### Data model and matching

`Ingredient` (item id / tag / `Any`), a memoised cycle-guarded `TagResolver`, `CraftingGrid` (row-major id snapshot) and `Recipe` (shaped, shapeless, cooking, stonecutting, two smithing kinds, transmute, hard-coded `Special`). A shaped pattern matches at **any offset** and by default **mirrored**, with every uncovered cell required empty (so a solid 3x3 of planks is not a chest). Shapeless matching is a **bipartite perfect matching**, not "each ingredient appears somewhere" (the naive version lets one item satisfy two slots).

`CorpusBuilder` is source-agnostic (`push_recipe`/`push_tag` take `(Identifier, &str)`) and records a malformed document in `failures()` instead of aborting, so one unknown future recipe type cannot leave the client with none. `load_data_root` walks `data/` **recursively** (a flat `read_dir` drops a third of item tags since tag ids are path-derived, `tags/item/enchantable/weapon.json` to `minecraft:enchantable/weapon`).

The JSON boundary is typed: `RecipeDocument` (tagged enum), `IngredientDocument` (recursive item/tag/choice), `ResultDocument` (compact string or counted object), `TagDocument`. Use `parse_recipe_document`/`parse_tag_document` for the DTO and `parse_recipe_text`/`parse_tag_text` for model entries; none accepts an untyped JSON value. `UnsupportedRecipeDocument` keeps the full source object so a newer kind stays visible and forwardable.

Slot order is the trap: window 0 is `0` result / `1..=4` craft / `5..=8` armour / `9..=35` main / `36..=44` hotbar / `45` off-hand, while a crafting table is `0` result / `1..=9` grid / `10..=36` main / `37..=45` hotbar (no armour or off-hand; hotbar not at 36). `MenuKind` stays `Generic` for a crafting table; branch on `craft_layout()`. Slot *kinds* matter: a plain slot at index 0 lets shift-click deposit into the result and desyncs prediction.

### Runtime recipe registration (plugin API)

`RecipeBook::register`/`unregister` are the validated counterparts of the loader's `insert`: `Err(Duplicate)` on a colliding id and `Err(ReservedNamespace)` for `minecraft:` (the loader silently replaces and allows either). `lodestone_ecs::recipes::RecipeRegistry` is the shared resource plugins call in `Plugin::build`; that runs before the corpus loads, so registrations stay **pending** and replay onto whichever corpus is adopted (order-independent). The shell re-clones the merged book only when `RecipeRegistry::revision` moves (one `u64` compare per frame instead of cloning a 1,585-recipe corpus). `RecipeBookSync` follows the same rule: its source revision advances per recipe-sync event and the shell borrows a `Sim`-owned snapshot until it changes.

Gotchas: `TagResolver`'s memo must be an `RwLock`, not a `RefCell` (`!Sync` would stop `RecipeBook` being a `bevy_ecs` `Resource`); `unregister` must go through `RecipeBook`, never a caller-side `Vec::remove` (a stale `grid_index` silently degrades an unrelated recipe's matching). Server-side plugin registration is not wired: the host's bundled corpus is independent, so a registered recipe is client prediction and recipe-book UI only against a real server.

### Recipe-book UI

- `RecipeBook::browse` substring-matches the **result item's id** (a deliberate simplification; there is no resolved-name index). Tabs come from each recipe's `"category"` and the reference per-book tab list (declaration order, not symmetric: a blast furnace has no Food tab, a smoker only Food).
- Auto-fill (`Recipe::placement` + `plan_auto_fill`) is the inverse of `match_grid`: ingredient to cell, always top-left, never mirrored. `plan_auto_fill` emits one step per grid cell and several steps can share a source slot, so a literal per-step pick-up/place dumps a whole stack into the first cell. The real sequence, grouped by source slot: pick up the stack, right-click ("place one") into each cell it supplies, pick it up again to return the remainder.
- `RecipeToastQueue` merges unlocks within 5 seconds into one cycling toast; the first sync of a session seeds the "already toasted" set without toasting. Toast geometry is transcribed from the reference records, not inferred from a call site (a Java record's positional fields were once transcribed backwards).
- Two unlock stores: `RecipeUnlockState` (keyed on an identifier) is dead and permanently reports every recipe unlocked, because the wire's recipe display id is a session-assigned integer with no identifier; the real tracking is `lodestone_game::recipe_sync::RecipeBookSync` keyed on the recipe display id.
- The server folds inbound recipe-book open/filter changes into `PlayerInventory::recipe_book_settings` (session-scoped until player data has an authoritative representation). It also owns per-connection "new" state: every entry in the initial `recipe_book_add` snapshot is highlighted without a toast; on `recipe_book_seen_recipe` it validates the opaque id against the advertised book, clears that highlight and returns a non-replacing one-entry update. Ids and acknowledgements are session-local and never persisted.

## How to change it

- Plugin validation belongs in `RecipeRegistration::validate`, not the ECS layer (transport only).
- A matcher feature: extend `Recipe`/`match_grid` in `recipe.rs` and check whether auto-fill (`Recipe::placement`) needs the inverse.
- The slot-order table is restated, not shared, between a generic container and a crafting table; check both when adding a screen kind.

## Configuration

Cargo feature `json` on `lodestone-game` (off by default) enables `recipe_json` and its typed serde/thiserror/bon boundary; the shell enables it (`lodestone-game = { workspace = true, features = ["json"] }`) to load the real corpus from `client.jar` at GPU bring-up. Corpus tests read the current release's `client-src/data` (gitignored) and are `#[ignore]`d.

## Dependencies

`lodestone-model` (`Identifier`, `ClientEvent`, `ItemStack`, `Text`); `serde`/`serde_json` (optional), `thiserror`, `bon`; `bevy_ecs` for the registration resource. Nothing is version-specific (recipes are `Identifier`-keyed). Consumed by `lodestone-client` (`Menus`) and `lodestone-shell` (`container.rs`, `resources.rs::load_recipe_book`, `app.rs`/`hud.rs`).
