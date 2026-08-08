# Server-side crafting

## What it is

The server half of crafting (issue #529): `crates/lodestone-server/src/crafting.rs`
holds the crafting grid the **server** owns plus the bundled 26.2 recipe corpus it
resolves a result from, and `apply_container_clicked` now derives the crafting
result slot itself instead of storing whatever the client claimed. The client-side
matcher and menu layout are `docs/crafting.md`.

## How it works

Three pieces, in the order they depend on each other.

**The grid.** `CraftingState` is vanilla's `CraftingContainer` + `ResultContainer`
pair — `width * height` input cells and one result. The player inventory screen's
2×2 lives on `PlayerInventory::crafting` (vanilla keeps it in `InventoryMenu`'s
scratch slots, but this crate's per-connection thing is the inventory). Every
input write goes through `CraftingState::set_input`, which re-derives the result
immediately; there is no way to mutate a cell and leave a stale result, because a
stale result is the same defect as a trusted one.

**The corpus.** `assets/recipe/` is vanilla 26.2's full `crafting_shaped` (733) +
`crafting_shapeless` (323) set and `assets/tags/item/` is the 224 item tags their
ingredients reference; `build.rs` embeds both as `EMBEDDED_RECIPES` /
`EMBEDDED_ITEM_TAGS` on the same `include_str!` pattern `assets/loot_table/`
already uses. `crafting::recipe_book()` parses them once into a
`lodestone_game::recipe::RecipeBook` behind a `OnceLock`.

**The authority.** Menu slot `0` of window `0` is the crafting result.
`PlayerInventory::apply_menu_slot_change` **refuses** it — it returns `false` and
stores nothing — while menu slots `1..=4` route into the grid. After any click
that touched the grid or named slot `0`, `apply_container_clicked` returns a
`container_set_slot` carrying the result *the server derived*, which
`dispatch_play_packet` sends on the same packet. So a diff claiming a diamond
block out of an empty grid mints nothing and the client is corrected immediately,
exactly as vanilla's `ResultContainer` broadcast does.

There is deliberately **no `consume_one`** on a take. A real take's diff already
carries the shrunk grid cells alongside the emptied result, so consuming again
would shrink the grid twice; the server applies the cells the client reports and
re-derives from those.

## What is not here

Issue #529's steps 2 and 4 are not landed and the issue is still open:

- **No crafting-table menu.** Nothing calls `CraftingState::table()` in
  production — a crafting table is not a block entity, so `open_container_screen`
  (which is driven by `BlockEntityHandle`) cannot open one, and a virtual
  positionless menu is its own landing.
- **`PLACE_RECIPE` is still discarded.** It needs a `ServerBound` variant, a v770
  decode arm, and grid-fill-from-inventory logic.

And the wider trust `apply_container_clicked` documents is unchanged: the server
still applies the client's own slot diff for ordinary inventory slots rather than
re-running vanilla's `doClick`. That is not crafting-specific and is the same
scope cut `docs/container-clicks.md` records. What #529 closed is the *result
slot*, which was the only place a diff could name an item the player never had.

## How to change it

**Refreshing the corpus**: re-copy `crafting_shaped` + `crafting_shapeless` from
`.cache/mc/26.2/src/data/minecraft/recipe/` and all of
`data/minecraft/tags/item/`, then update `BUNDLED_CRAFTING_RECIPES`. Both halves
or neither — an ingredient spelled `#minecraft:planks` matches nothing without its
tag document, and a **partial** corpus is worse than none: it rejects *valid*
crafts while every recipe that is present still works, so no test that only asks
"does crafting work" would notice. That is what the count constant is for.

**Adding the table menu**: `CraftingState::table()` is already the 3×3, and
`crafting::CraftingState` is `Clone + PartialEq` so it can hang off
`OpenContainer`. The work is the menu-open path, not the matching.

**Do not write a second matcher.** The client's is version-free game logic and the
server calls the same one; two independent readings of the recipe format is how the
two ends drift apart.

## Configuration

None. The corpus is compile-time embedded and has no runtime switch.

## Dependencies

- `lodestone-game` (its `json` feature) for `recipe::RecipeBook`,
  `recipe::CraftingGrid` and `recipe_json::CorpusBuilder`. This revisits the
  `Cargo.toml` note about keeping `lodestone-game` out of this graph; the reason
  it is now the right call is measurable — `lodestone-game`'s own dependencies are
  `lodestone-model` + `uuid`, so the browser bundle gains that crate's code and no
  new transitive graph. `cargo xtask check-isolation` reports no new edge.
- `serde_json`, already present, via that feature.
