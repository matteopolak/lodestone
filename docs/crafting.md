# Crafting

## What it is

The version-free crafting stack in `lodestone-game`: the recipe data model and
matching rules (`recipe.rs`), a loader for Mojang's own datapack JSON
(`recipe_json.rs`), and the crafting-table menu layout that `menus.rs` builds
when the server opens a `minecraft:crafting` screen.

## Who computes the result slot

**The server does.** Vanilla's `CraftingMenu.slotsChanged` runs server-side,
computes the result and pushes it as a `container_set_slot` for slot 0; the
vanilla client never matches recipes to fill that slot. This client does the
same — `Menus::apply` already routes `ContainerSlot` through
`ClientMenu::reconcile`, so an open crafting table shows the correct result with
no local corpus at all.

A local corpus is therefore **not** on the critical path for "put items in the
grid, see the result". It is needed for:

- the recipe book UI (what can I make?) and ghost recipes,
- showing a result before the server round-trip lands,
- anything offline.

Related: since 1.21.2 the `update_recipes` packet no longer carries the crafting
corpus. It carries *recipe property sets* (which items are valid furnace inputs,
etc.) and the stonecutter list. The recipe **book** arrives via
`recipe_book_add` as display-only `RecipeDisplay` entries. Neither is a
substitute for the datapack corpus, and as of this writing the v770 adapter
decodes neither (see "Not wired yet").

## How it works

### Data model — `crates/lodestone-game/src/recipe.rs`

- `Ingredient` — an item id, a `#tag`, or `Any` of several options.
- `TagResolver` — flattens (possibly nested) item tags, memoised, cycle-guarded.
- `CraftingGrid` — a `w × h` row-major snapshot of grid contents, ids only.
- `Recipe` — `Shaped`, `Shapeless`, `Cooking`, `Stonecutting`, the two smithing
  kinds, `Transmute`, and `Special` (hard-coded vanilla recipes with no
  data-driven ingredients). Only the first two implement `match_grid`.
- `RecipeBook` — the aggregate: id-sorted recipes plus their `TagResolver`.
  `match_grid` returns the first match in id order.

Two matching rules that are easy to get subtly wrong, and are covered by tests:

- A shaped pattern matches at **any offset** in the grid and, by default,
  **mirrored** left-to-right. Cells the pattern does not cover must be empty —
  that is what makes a full 3×3 of planks *not* a chest.
- Shapeless matching is a **bipartite perfect matching**, not "each ingredient
  appears somewhere". The naive version accepts a grid that reuses one item to
  satisfy two ingredients.

### Loading — `crates/lodestone-game/src/recipe_json.rs` (feature `json`)

`CorpusBuilder` is source-agnostic: it takes `(Identifier, &str)` pairs via
`push_recipe` / `push_tag` and knows nothing about files or jars. A malformed
document is recorded in `failures()` rather than aborting the load, so one
unknown recipe type from a future version cannot leave the client with no
recipes.

`load_data_root(&Path)` layers a filesystem walk on top. Point it at a datapack
`data/` root — in practice the `data/` directory inside **`client.jar`**:

```
data/minecraft/recipe/**/*.json
data/minecraft/tags/item/**/*.json
```

The walk is **recursive** and the id is the path relative to `recipe/` or
`tags/item/` minus `.json`, so `tags/item/enchantable/weapon.json` becomes
`minecraft:enchantable/weapon` — the rule vanilla's `FileToIdConverter` uses. A
flat `read_dir` silently drops 33 of 26.2's 224 item tags.

### Crafting menus — `menu.rs` / `menus.rs`

`Menu::crafting(3, 3)` builds vanilla's `CraftingMenu`: slot `0` is `Output`
(take-only), `1..=9` are `CraftingInput`, `10..=36` main storage, `37..=45`
hotbar. `Menu::craft_layout()` reports where the grid and result live;
`Menu::crafting_grid()` snapshots it into a `CraftingGrid`. `Menu::player()`
reports the same for its 2×2.

`Menus::build_menu` picks the layout from the `open_screen` menu type, but the
**size always comes from the server** (`content_len - 36`); a `minecraft:crafting`
screen whose content length is not 46 falls back to a generic container rather
than building a menu the packet cannot fill.

`Menus::predicted_craft_result(&RecipeBook)` matches the active menu's grid
against a book. It is explicitly a *prediction* — never write it into the result
slot, which the server owns.

## Gotchas

- **Slot order.** Window 0 is `0` result, `1..=4` craft, `5..=8` armour, `9..=35`
  main, `36..=44` hotbar, `45` off-hand. A crafting table is `0` result, `1..=9`
  grid, `10..=36` main, `37..=45` hotbar — **no armour, no off-hand, and the
  hotbar is not at 36**. Getting this wrong renders a plausible but transposed
  inventory that reads as an art bug.
- **`MenuKind` stays `Generic`** for a crafting table. Positionally it *is* a
  generic container of 10 leading slots; only the slot kinds differ, and those
  live on `Slot`. Branch on `craft_layout()`, not on `MenuKind`, when you need to
  know a menu crafts.
- **Slot kinds are load-bearing.** With a plain `Normal` slot at index 0, a
  shift-click from the player inventory deposits into the *result* slot and the
  server contradicts every prediction that follows.
- `menu_type` is only known if `open_screen` arrived before the content packet.
  `Menus` handles a content packet for a window it never saw opened by building a
  generic menu with unknown metadata.

## Configuration

- Cargo feature `json` on `lodestone-game` (off by default) enables
  `recipe_json`; it pulls in `serde_json` only.
- The corpus tests read `.cache/mc/26.2/client-src/data`, which is gitignored,
  so they are `#[ignore]`d:

```
cargo test -p lodestone-game --features json --test recipe_book -- --ignored --nocapture
cargo test -p lodestone-game --features json --test recipe_corpus -- --ignored --nocapture
```

## Wiring into the shell

`crates/lodestone-shell/src/container.rs`'s `slot_layout` has, since before
this section was last accurate, dispatched additively on `Menu::craft_layout()`
(`crafting_layout`): a crafting table draws the real 3×3 grid plus the
take-only result slot, not a flat 9-wide generic run. See that file's own
module doc for the layout constants and the slot-order gotchas.

`crates/lodestone-shell/src/resources.rs::load_recipe_book` feeds the real
corpus in: it opens `client.jar` the same way every other shell asset loader
does (`ZipSource` + `ResourceManager`), lists everything under `data/` with
`ResourceManager::list` (a plain prefix match — nesting-safe, unlike a
filesystem walk, since a prefix filter has no notion of depth to get wrong),
and feeds each `recipe/**` and `tags/item/**` entry to `CorpusBuilder`. It does
**not** call `load_data_root`: that function walks a real filesystem
directory, and the corpus here lives inside a zip. `WindowApp` (`app.rs`)
loads it once at GPU bring-up into a `recipe_book: Option<RecipeBook>` field.

**Requires the `json` feature on `lodestone-game`'s dependency edge from
`lodestone-shell`**, which is *not yet enabled* in
`crates/lodestone-shell/Cargo.toml` (still `lodestone-game = { workspace =
true }`, no `features = ["json"]`) — `recipe_json` is behind that feature and
gates on the *depending* crate's own Cargo.toml, not a runtime check. Until
that one line is added, `resources.rs` does not compile at all, let alone load
recipes. This is a deliberate, narrow gap: it was left for review rather than
made by the same change that added `load_recipe_book`, per that session's file
scope.

The loaded book feeds two things, both additive and neither touching the
result slot's server authority:

- **A ghost preview.** `ContainerFrame::with_recipe_book` attaches the book;
  when the crafting result slot is empty, `ContainerGeometry::build_inner`
  matches `menu.crafting_grid()` against it directly (not through
  `Menus::predicted_craft_result`, which needs the `Menus` wrapper the shell's
  `Sim` deliberately does not expose past `Menu` snapshots — see `sim.rs`'s
  `player_menu`/`open_menu`) and draws the predicted result dimmed. The match
  reruns every frame against live `menu` state and writes nothing back into
  it, so a server disagreeing just means the real `slot_item` draw takes over
  next frame — the same "server truth always wins" contract every other slot
  already has.
- **A debug-overlay counter.** `HudFrame::recipe_stats` appends a
  `recipes=N tags=M` line to the F3 overlay when the book has loaded, as the
  cheap "did this actually reach the running client" signal — the same role
  `assets.sprites`/`assets.items` counters play for the other loaders in
  `resources.rs`.

Remaining gaps:

- The v770 adapter encodes the serverbound `place_recipe`,
  `recipe_book_change_settings` and `recipe_book_seen_recipe` packets, but
  decodes **none** of the clientbound recipe packets (`update_recipes`,
  `recipe_book_add`/`remove`/`settings`, `place_ghost_recipe`). There is no
  `ClientEvent` for them yet.
- No recipe-book UI (the "what can I make" browser) exists; the local corpus
  is currently consumed only by the ghost preview and the debug counter above.

## Dependencies

- `lodestone-model` — `Identifier`, `ClientEvent`, `ItemStack`, `Text`.
- `serde_json` (optional, feature `json`).
- Consumed by `lodestone-client` (`state.rs` owns a `Menus`), which the shell
  reads through `ClientHandle::player_menu` / `open_menu`.
