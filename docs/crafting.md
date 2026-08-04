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
`lodestone-shell`.** This section previously said that edge was *not yet
enabled* and named the exact missing line — stale as of issue #163:
`crates/lodestone-shell/Cargo.toml:35` is now `lodestone-game = { workspace =
true, features = ["json"] }`. `resources.rs::load_recipe_book` compiles and
the corpus really does load into the running client; verify with `grep -n
'features = \["json"\]' crates/lodestone-shell/Cargo.toml` before trusting
this paragraph either, per this repo's own staleness rule.

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
  `ClientEvent` for them yet. **Confirmed again for issue #163**: `grep -n
  "PLACE_GHOST_RECIPE\|RECIPE_BOOK_ADD\|RECIPE_BOOK_REMOVE\|RECIPE_BOOK_SETTINGS\|UPDATE_RECIPES"
  crates/protocol/v770/src/adapter.rs` returns zero hits, even though the
  packet-id constants themselves exist in `generated/packet_ids.rs` (that
  file only proves the *id* is known, not that anything decodes it — see
  `docs/README.md`'s own connectedness caveats). This is why "recipe-unlock
  tracking" below is real scaffolding with no live producer yet.

## Recipe-book UI (issue #163)

The browsing/unlock UI layered on top of the matcher above, entirely in
`lodestone-game` (`recipe.rs`, `menu.rs`) and `lodestone-shell`
(`container.rs`). It does **not** duplicate `RecipeBook::match_grid` — see
"Auto-fill" below, which reuses it via a new inverse operation
(`Recipe::placement`), not a second matcher.

### Categories and browsing — `recipe.rs`

`RecipeCategory` (`Building`/`Redstone`/`Equipment`/`Food`/`Blocks`/`Misc`)
captures each recipe JSON's own `"category"` field — real per-recipe data
(694 of 1585 recipes in 26.2 carry one; `recipe_json.rs`'s `parse_category`
parses it), not a heuristic. `tabs_for(RecipeBookType)` is vanilla's own
per-book tab list in **declaration order**
(`RecipeBookCategories.java:7-19`), which is not alphabetical and is not
symmetric across appliances: `BlastFurnace` has no `Food` tab and `Smoker`
has *only* `Food`. `RecipeBook::visible_tabs` filters that list down to
categories with at least one loaded recipe, mirroring
`RecipeBookTabButton.updateVisibility`.

`RecipeBook::browse(book_type, category, search)` returns matching recipe
ids in the corpus's own id order. **This is a deliberate simplification**,
not vanilla's real search: vanilla fuzzy-matches a `SearchTree` built from
the resolved item's tooltip/display name
(`ClientPacketListener.searchTrees()`); this client has no resolved-name
index to build one from, so `browse` substring-matches the **result item's
id** (namespace and bare path) instead. A query like `"planks"` still finds
every planks recipe, just not by its translated display name.

### Panel geometry — `container.rs`

`RecipeBookPanelLayout`/`recipe_book_panel_layout[_with_scale]` and
`RecipeBookPanelHit`/`recipe_book_panel_hit_test[_with_scale]` mirror every
other pair in this module (`panel_origin`/`hit_test`): pure functions, no
GPU/asset dependency, unit-tested with predicted rects. Every constant is
transcribed from `RecipeBookComponent`/`RecipeBookPage`/`RecipeBookTabButton`/
`RecipeButton.java` with a `file:line` citation in the source.

**One gap, kept deliberately unfixed**: vanilla shifts the *main* container
screen rightward when the book opens
(`RecipeBookComponent.updateScreenPosition`, `:173-182`) so the two panels
never overlap. Replicating that would mean threading an "is the book open"
flag through `panel_origin`/`hit_test`/`ContainerGeometry::build_inner` —
functions every container screen calls, not just these two — for a change
scoped to one feature. Instead the book panel's left edge is clamped to a
`4px` floor and may overlap the main panel at narrow canvases rather than
displacing it. Visible, bounded, and documented rather than fixed by
touching shared rendering code with no isolated way to verify the blast
radius.

`recipe_book_panel_contents` is the separate data query (`RecipeBook` in,
`RecipeBookPanelContents` out: visible tabs, this page's ids, page count) —
kept apart from the layout function on purpose, so geometry never needs a
loaded `RecipeBook` and the data query never needs a viewport size.

### Recipe-unlock tracking — `RecipeUnlockState` (`recipe.rs`)

**Nothing populates this today.** The server signal is
`recipe_book_add`/`recipe_book_remove`, and per the "Remaining gaps" section
above, `v770`'s adapter decodes neither — confirmed by grep, not assumed —
nor does `lodestone-model` have a `ClientEvent` for them. Both are outside
this change's owned files (`crates/protocol/**`); see "Brokered work" below.

Until that lands, `RecipeUnlockState::is_unlocked` reports **every** recipe
as unlocked, so the browsable panel shows the full local corpus rather than
an empty one — a visible, honest stand-in for missing data, not a silently
fake "everything is unlocked" that would survive real data arriving. The
moment a single `unlock`/`remove` call is made (`has_data()` flips true) it
switches to the real per-id answer. `unlock`/`remove`/`take_new` (for the
toast, next) are implemented and unit-tested against direct calls; nothing
in the running client calls them yet.

### Unlock toast — `RecipeToastQueue` (`recipe.rs`)

Pure timing data mirroring `RecipeToast.java`: `RECIPE_TOAST_DISPLAY_MS =
5000` (`:17`, **100 ticks** at the fixed 50ms tick), width `160`/height `32`
(`Toast.java:14-15`). Multiple recipes unlocked within the window merge into
one toast that **cycles** through them (`displayed_entry`, mirroring
`RecipeToast.java:49-51`'s formula) rather than stacking separate toasts.
Nothing calls `push` from live data yet — same blocker as unlock tracking —
and nothing renders it: that is `hud.rs`, brokered (below).

### Auto-fill — `Recipe::placement` + `plan_auto_fill` (`recipe.rs`), `Menu::plan_recipe_auto_fill` (`menu.rs`)

Reuses the existing matcher's own data (`Ingredient`/`TagResolver`), not a
new one. `Recipe::placement(grid_w, grid_h)` is the **inverse** of
`match_grid`: given a recipe, which ingredient goes in which cell — always
top-left, never mirrored (vanilla places at the position its own
`RecipeDisplay` carries, which is not decoded here — a documented
simplification, not a guess dressed up as the real position).
`plan_auto_fill` then greedily matches each required cell against a
caller-supplied inventory snapshot, one physical stack per cell, never
overdrawing a stack past its own count, and refusing (`None`) rather than
partially filling a grid it cannot complete. It deliberately does **not**
model `use_max_items` (shift-click "craft as many as possible").

`Menu::plan_recipe_auto_fill` is the thin per-menu wrapper: it reads the
menu's own `craft_layout`/`special_layout` to know whether this is a
crafting-table grid or a furnace-family single ingredient slot, scans
**main storage and hotbar only** (never armour/off-hand — matching
vanilla's own `PlaceRecipeHelper`, which only ever walks
`Inventory.items`), and returns steps already offset to absolute menu-slot
indices.

**What still turns a plan into pixels is brokered, not done**: the plan is a
`Vec<PlacementStep>` (`{cell: menu_slot, source_slot: menu_slot}`), not a
network action. There is no existing "move exactly one item from slot A to
slot B" primitive without introducing wire-level stack accounting, so the
dispatch loop (issue two `ContainerClick`s per step, using the same
per-click prediction/dispatch every other manual slot click already goes
through) belongs in `app.rs`/`sim.rs` — see "Brokered work" below. Sending
vanilla's own `PlaceRecipe` action instead is not an option today: its wire
field is a **`RecipeDisplayId`**, a server-session-assigned integer from the
undecoded `recipe_book_add` packet, not this corpus's `Identifier` — so this
client cannot construct a correct one without that same decode.

### Brokered work

None of the following is implemented — `crates/protocol/**` is off-limits to
this change, and `app.rs`/`sim.rs`/`hud.rs` are choke-point files this repo
brokers through file-owner review rather than editing directly:

1. **Protocol decode** (`crates/protocol/v770/src/adapter.rs`): clientbound
   arms for `recipe_book_add`/`recipe_book_remove`/`recipe_book_settings`,
   plus a `ClientEvent` variant in `lodestone-model` to decode into and a
   `net.rs`/ingest consumer that calls `RecipeUnlockState::unlock`/`remove`.
2. **Toggle wiring** (`app.rs`): one bool (or small struct, see
   `container.rs`'s `RecipeBookPanelLayout`) for panel-open state, a call to
   `recipe_book_panel_hit_test_with_scale` alongside the existing
   `hit_test_with_scale` call, and drawing the panel's own geometry.
3. **Click-to-fill dispatch** (`app.rs`/`sim.rs`): turning a
   `Menu::plan_recipe_auto_fill` plan into a sequence of `ContainerClick`
   actions through the existing single-click pipeline.
4. **Toast rendering** (`hud.rs`): `RecipeToastQueue::displayed_entry` into
   an on-screen toast, at the size/position `Toast.java`'s own
   `xPos`/`yPos`/`width`/`height` describe.

Tracked on [#436](https://github.com/matteopolak/lodestone/issues/436).

## Dependencies

- `lodestone-model` — `Identifier`, `ClientEvent`, `ItemStack`, `Text`.
- `serde_json` (optional, feature `json`).
- Consumed by `lodestone-client` (`state.rs` owns a `Menus`), which the shell
  reads through `ClientHandle::player_menu` / `open_menu`.
