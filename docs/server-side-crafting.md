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

## The crafting-table menu is a *positionless virtual* menu (step 2)

`open_container_screen` structurally cannot open a crafting table: it is driven
entirely by a `BlockEntity` found at `pos`, and **a crafting table is not a block
entity.** Vanilla's `CraftingMenu` builds a `TransientCraftingContainer` +
`ResultContainer` in its constructor and throws them away on close.

So `open_crafting_table_screen` is the second open path. The grid lives on
`PlayerInventory::table_crafting` — `Some` exactly while the menu is open, which is
what makes "is this window a crafting table" answerable without a second registry —
and `OpenContainer::shape` is `MenuKind::CraftingTable`. `pos` is still carried, but
only so breaking the table closes the window, exactly as it already does for a
furnace.

Closing the menu returns the grid **and** the cursor to the player
(`take_table_crafting` + `ClickState::reset`), with the overflow dropped in the
world. A grid silently discarded on close deletes items on every close.

## `PLACE_RECIPE` (step 4): the server half is here, and nothing sends it yet

`ServerBound::RecipePlaced` decodes, and `apply_recipe_placed` →
`crafting::place_recipe` is vanilla's `ServerPlaceRecipe`: the grid goes back to the
player, then one ingredient per pattern cell is taken out of `items` (`0..36` —
never armour or the off-hand). `use_max_items` runs as many all-or-nothing rounds as
the inventory allows; a partially-filled extra round would leave a shape matching no
recipe.

**`RecipeDisplayId` is an opaque index the *server* assigns**, not a recipe name:
vanilla sends the whole book with `ClientboundRecipeBookAddPacket` and the client
echoes back a position in that list. This crate does not encode that packet, so
`crafting::recipe_at_index` defines the index as the bundled corpus's own id-sorted
order — the order such an encoder would emit, so the two cannot disagree once it
exists. Until it does, **no client learns an id and no client sends a valid
`PLACE_RECIPE`**: this half is complete and unreachable. Closing that needs
`encode_recipe_book_add` in `lodestone-v770` (the `RecipeDisplay`/`SlotDisplay`
hierarchy) plus, for *our own* client, a decode in `lodestone-shell`, which already
has four separate notes saying that decode does not exist.

## What is not here

The wider container-diff trust `apply_container_clicked` used to document is
**closed** — see `docs/server-inventory.md`'s "the click is derived, not trusted".
Both the result slot and every ordinary slot are now server-derived.

## How to change it

**Refreshing the corpus**: re-copy `crafting_shaped` + `crafting_shapeless` from
`.cache/mc/26.2/src/data/minecraft/recipe/` and all of
`data/minecraft/tags/item/`, then update `BUNDLED_CRAFTING_RECIPES`. Both halves
or neither — an ingredient spelled `#minecraft:planks` matches nothing without its
tag document, and a **partial** corpus is worse than none: it rejects *valid*
crafts while every recipe that is present still works, so no test that only asks
"does crafting work" would notice. That is what the count constant is for.

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
