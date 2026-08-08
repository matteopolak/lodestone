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
stores nothing — while menu slots `1..=4` route into the grid. `do_click` is then
re-run server-side from `(slot, button, click_type)` and the result slot is
re-derived from the grid, never copied out of the click. So a diff claiming a
diamond block out of an empty grid mints nothing.

## Taking the result: write-protected is not un-clickable

The rule "the result slot is never written by anything the client sent" is right, and
reading it as "the client cannot interact with slot 0" is what produced a shipped
bug — three symptoms, one cause. Vanilla's `ResultSlot.mayPlace` is `false` and its
`onTake` is what decrements the grid: **a click on slot 0 is how you craft.**

Two properties make that work, and both are now in
`crates/lodestone-server/src/container_click.rs`:

* **`slotsChanged` runs inside the click.** `CraftingMenu.slotChangedCraftingGrid`
  re-derives slot 0 and pushes a `container_set_slot` on *any* grid change, so the
  result slot is live for the duration of one `doClick`. `do_click_with` takes the
  menu's corpus (`crafting::derive_result` bound to the open grid's dimensions) and
  re-derives after every grid mutation; plain `do_click` keeps the recipe-free
  behaviour for callers with no corpus.
* **`QUICK_MOVE` repeats.** `AbstractContainerMenu.doClick` loops `quickMoveStack`
  *while the clicked slot still holds the same item*. For every slot but a result that
  runs once; for a result it is the whole of "shift-click crafts until the grid runs
  out", because the loop only terminates once the refill stops. It ends on an empty
  grid or a full inventory (nothing moves → `quickMoveStack` returns EMPTY).

### The resync was the actual defect

`apply_container_clicked` answers a click with a corrective `container_set_content`
when the client's prediction disagrees with what the server derived. That comparison
used to walk **only the slots the client claimed** — and a client claims nothing for
a change it cannot predict, which is precisely the server-derived result slot. So
every crafting click "agreed", nothing was sent, and:

| symptom | mechanism |
|---|---|
| the output draws dimmed and looks unclickable | the client's result slot stayed empty, so the container screen drew its own *ghost preview* (`ContainerFrame::with_recipe_book`) and a click on an empty local slot predicted nothing |
| shift-clicking the output needs a close+reopen | the craft really happened server-side; the only packet that ever carried it was the next full `container_set_content`, i.e. the next open |
| items appear at unrelated moments | once client and server disagree about a *native* slot (an invisible craft landed in one), the next click whose `moveItemStackTo` picks a different destination finally disagrees and forces the resync |

The comparison is now "the pre-click state, overwritten by what the client claimed,
must equal the whole derived menu" — vanilla's `remoteSlots` diff in
`broadcastChanges`, which is unconditional and per-slot. An honest prediction still
costs no traffic; the control for that is the second half of
`a_derived_result_is_pushed_to_the_client_and_an_honest_claim_still_costs_nothing`.

**A take does consume the grid**, and an earlier note here said the opposite ("no
`consume_one`, the client's diff already carries the shrunk cells"). That was true
only while the client's diff was trusted. It is not, so `ResultSlot.onTake` is ours to
perform.

One datum worth keeping because it reads as a bug in play: **a single plank in the 2×2
is a real recipe** (`oak_button.json`), and two side by side are
`oak_pressure_plate.json`. Filling a grid one click at a time therefore changes the
result on *every* click, and each one is a packet.

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

## `PLACE_RECIPE` (step 4): reachable, because the book is now sent

`ServerBound::RecipePlaced` decodes, and `apply_recipe_placed` →
`crafting::place_recipe` is vanilla's `ServerPlaceRecipe`: the grid goes back to the
player, then one ingredient per pattern cell is taken out of `items` (`0..36` —
never armour or the off-hand). `use_max_items` runs as many all-or-nothing rounds as
the inventory allows; a partially-filled extra round would leave a shape matching no
recipe.

### The id space, and the packet that hands it out (issue #547)

**`RecipeDisplayId` is an opaque index the *server* assigns**, not a recipe name:
vanilla sends the whole book with `ClientboundRecipeBookAddPacket` and the client
echoes back a position in that list. That packet is now encoded, so `PLACE_RECIPE` is
reachable rather than structurally dead.

* `crafting::recipe_book_entries()` builds the whole bundled corpus as
  `RecipeBookEntry` values, walking **the same id-sorted order**
  `crafting::recipe_at_index` resolves. One index space by construction, and
  `crates/protocol/v770/tests/recipe_book_add.rs`'s
  `every_entry_id_resolves_to_the_same_recipe` asserts it — a drift here places a
  *different* recipe on every click, silently and plausibly.
* `ServerProtocol::encode_recipe_book_add` is the seam; `lodestone-v770` implements
  it. `server.rs` sends it once at the Configuration→Play handoff with
  `replace: true`, beside the advancements packet.
* Only `crafting_shaped` and `crafting_shapeless` displays exist. The corpus this
  crate bundles is those two recipe types, so a furnace/stonecutter/smithing display
  would have nothing to describe.

Three wire details worth keeping, each a place a transcription goes wrong:

| field | shape |
|---|---|
| `SlotDisplay` dispatch | `SlotDisplays.bootstrap` **registration order**: `empty` 0, `item` 4, `item_stack` 5, `tag` 6, `composite` 10 |
| `ItemStackTemplate` | item **then** count — the *opposite* order from `ItemStack.OPTIONAL_STREAM_CODEC`, which leads with the count |
| `Ingredient` (`HolderSet<Item>`) | a VarInt of `0` means "a tag reference follows"; `n + 1` means "`n` direct entries follow". Every list we emit is direct, so every count is `len + 1` |

An item id the 26.2 census cannot resolve degrades to `empty` rather than writing a
guessed id: a wrong id desyncs the whole 1,000-entry list, which surfaces as a
disconnect about some unrelated later field.

**Our own client still does not decode it.** `lodestone-shell` has four separate
notes saying so, and its recipe-book screen is fed from the client's own jar corpus.
That is a client-side gap, not a server one — a real vanilla 26.2 client is a genuine
consumer of this packet today.

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
