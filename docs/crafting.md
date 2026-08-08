# Crafting

## What it is

The version-free crafting stack in `lodestone-game`: the recipe data model and
matching rules (`recipe.rs`), a loader for Mojang's own datapack JSON
(`recipe_json.rs`), and the crafting-table menu layout that `menus.rs` builds
when the server opens a `minecraft:crafting` screen.

> **Our own server does this now too** (issue #529) — see
> [`server-side-crafting.md`](./server-side-crafting.md). Until that landing it
> stored whatever result the client claimed.

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
  `recipe_book_change_settings` and `recipe_book_seen_recipe` packets. It
  decodes **one** of the five clientbound recipe packets: `recipe_book_settings`
  (76), which landed in `fd53995` as `ClientEvent::RecipeBookSettingsChanged`.
  The other four — `update_recipes`, `recipe_book_add`, `recipe_book_remove`,
  `place_ghost_recipe` — still have no decode and no `ClientEvent`.

  **The blanket "zero hits" claim that stood here was true when written for
  issue #163 and went stale the moment `fd53995` landed.** Re-run the grep
  rather than trusting this paragraph:

  ```
  grep -n "PLACE_GHOST_RECIPE\|RECIPE_BOOK_ADD\|RECIPE_BOOK_REMOVE\|RECIPE_BOOK_SETTINGS\|UPDATE_RECIPES" \
      crates/protocol/v770/src/adapter.rs
  ```

  The packet-id constants in `generated/packet_ids.rs` prove only that the *id*
  is known, never that anything decodes it — see `docs/README.md`'s own
  connectedness caveats.

- **The four undecoded packets are not blocked on the packets.** Their shared
  prerequisite is a recursive `SlotDisplay` decoder (11 registry-dispatched
  variants, one carrying a `DataComponentPatch` whose field order differs from
  `ItemStack.OPTIONAL_STREAM_CODEC`, one carrying a `Holder<TrimPattern>` whose
  `0` discriminator is an inline definition containing a full chat `Component`)
  plus a `RecipeDisplay` dispatcher (5 variants). Recursion is unbounded on the
  wire and vanilla does not bound it, so a depth cap is required. Measured for
  issue #436: `grep -rn "SlotDisplay\|RecipeDisplay" --include="*.rs" crates/`
  returns **5 hits, every one of them prose in a doc comment** — not a line of
  the codec exists. Estimate 400–600 lines.

  Ahead of the codec there is a **design blocker**, and it is the reason
  "the consumer is already built" is only half true: `RecipeUnlockState::unlock`
  and `remove` key on `Identifier` (`recipe.rs:1304`/`:1312`), the wire carries
  a `RecipeDisplayId` — a session-assigned integer (`v770/src/packets/game.rs:454`,
  `:467`) — and a `RecipeDisplay` contains no recipe id at all. So decoding
  `recipe_book_add` does not by itself let anything call `unlock`. Either the
  event carries the index plus a resolved result and something owns the
  index→`Identifier` map, or `RecipeUnlockState` gains an index-keyed path.
  `recipe_book_remove` is trivial to decode and **useless alone**, because that
  mapping arrives only in `recipe_book_add`. The toast renderer and its
  `app.rs`/`hud.rs` wiring *are* done; it is the key type that does not match.

### Recipe-book settings round trip (issue #436)

`recipe_book_settings` (76) folds into
`lodestone_ecs::session::SessionRecipeBookSettings`, and as of this section the
shell **reads** it: `WindowApp::restore_recipe_book_settings`
(`app/session.rs`, called every frame from `drive_ui_from_session`) applies
`RecipeBookSettings::for_type(book_type)` to `RecipePanelState`, so a player who
left the book open comes back to it open instead of always starting closed and
unfiltered.

Three things are easy to get wrong here:

- **`reported` is not decoration.** An unreported record is all-`false`, which
  is byte-identical to "the server wants it closed". Restoring without checking
  `reported` restores *our own default* — a wire that looks connected and
  carries nothing.
- **The restore must not report back.** The two click arms
  (`handle_recipe_panel_click`'s `Toggle` and `FilterButton`) call
  `Sim::send_recipe_book_settings`; the restore deliberately does not, or the
  server's own value would echo straight back at it.
- **Settings are per book type, `RecipePanelState` is one shared instance.** The
  latch is `restored_type: Option<RecipeBookType>`, not a `bool`, so opening a
  furnace after a crafting table restores again with the furnace's values —
  while still not re-restoring every frame and fighting the user's clicks.

`Sim::send_recipe_book_settings` is also the **first producer anywhere outside
`crates/protocol/`** of `ClientAction::SetRecipeBookSettings`: all four families
encoded it and nothing ever constructed one, the same outbound-island shape
`ClientAction::SetFlying` was caught in.

The All/Craftable filter is now modelled too (it previously was not, and
`RECIPE_SPRITE_FILTER` carried a doc comment saying the disabled art was the
only reachable state). `RecipePanelState::filtering` drives both the button art
and `recipe_book_panel_contents_filtered`, whose predicate is built from
`Menu::plan_recipe_auto_fill` — **the same primitive the click path uses**, so a
Craftable-filtered panel can never offer a recipe whose click would then refuse
to place it. The predicate runs over the whole browsed corpus rather than the
visible page, because pagination has to be computed from the filtered set; it is
only evaluated when `filtering` is set, so the cost is paid only in the state
that asks for it.

## Recipe-book UI (issue #163)

The browsing/unlock UI layered on top of the matcher above: the data and
geometry in `lodestone-game` (`recipe.rs`, `menu.rs`) and `lodestone-shell`
(`container.rs`), and the wiring that puts it on screen in `app.rs`/`hud.rs`
(see "Shell wiring" below). It does **not** duplicate
`RecipeBook::match_grid` — see "Auto-fill" below, which reuses it via a new
inverse operation (`Recipe::placement`), not a second matcher.

The panel, the click-to-fill and the toast render are all live. The one
remaining gap is the **protocol decode**, so unlock state and toast content
have no live producer yet; both degrade visibly rather than silently (see
"Still brokered").

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

#### The toggle button's position is per-screen, and getting this wrong shipped

`getRecipeBookButtonPosition` is **abstract with no default**
(`AbstractRecipeBookScreen.java:36`) and all three book-bearing screen families
override it with a different answer. The first cut of this panel used the
crafting table's for every screen, and the owner found the button in the wrong
place in the player inventory — 99 px left and 27 px above vanilla's, landing on
the armour column.

Local offsets off `(leftPos, topPos)`, with `topPos = (height - imageHeight)/2`
(`AbstractContainerScreen.java:78`) and `imageHeight` the `176x166` default for
all three, so the screen height and `leftPos` cancel:

| screen | jar expression | local |
|---|---|---|
| `InventoryScreen.java:64` | `(leftPos + 104, height/2 - 22)` | `(104, 61)` |
| `CraftingScreen.java:27` | `(leftPos + 5, height/2 - 49)` | `(5, 34)` |
| `AbstractFurnaceScreen.java:44` | `(leftPos + 20, height/2 - 49)` | `(20, 34)` |

`recipe_toggle_local` dispatches on `background_kind`, not a second `match` on
`special_layout`/`kind` — that function already answers "which vanilla screen
class is this menu", including the trap that a special-layout menu is
mechanically a `MenuKind::Generic`.

**Do not conflate this with the screen-shift gap above.** The local offset is
*invariant* to the shift: `AbstractRecipeBookScreen.java:42-44` re-derives the
button position off the already-shifted `leftPos`, and `topPos` is never
re-derived at all. So the button's position was a plain per-screen-constant bug
and the screen shift remains deliberately unfixed.

### Panel art — the real 26.2 textures

The panel first shipped drawing **flat fill colours** with no textures at all,
which the owner reported as "completely incorrectly textured". It was not
slightly off: vanilla's page is an opaque **white** sheet and our fill was
near-black.

Vanilla's art comes from two places, and the split matters:

- **The page** is a raw texture path, not a sprite:
  `RECIPE_BOOK_LOCATION = "textures/gui/recipe_book.png"`
  (`RecipeBookComponent.java:59`), blitted as a fixed `147x166` window at
  `(1, 1)` of a `256x256` sheet (`:305`). The one-pixel inset is real — decoding
  the PNG shows its opaque region is exactly `x 1..147, y 1..166`. It has **no**
  `.mcmeta` and is not nine-sliced (the only recipe-book sprite that is, is
  `overlay_recipe`, which this client does not draw).
- **Everything else** — `recipe_book/button`, `tab`, `tab_selected`,
  `filter_disabled`, `page_forward`, `page_backward`, `slot_craftable` — lives
  under `gui/sprites/recipe_book/**` and was therefore **already** in the atlas:
  `GuiAtlas::build` stitches every `assets/<ns>/textures/gui/sprites/**.png` in
  the pack. Wiring the art needed no new atlas, pipeline or bind group.

So the geometry carries `Vec<RecipeBookSprite>` — **ids and destination rects
only, no UVs and no atlas**. The producer runs with no GPU and no `GuiAtlas` in
scope, and `HudRenderer::render_recipe_book_panel` resolves each against
whatever atlas is bound, skipping unknown ids. The page needed
`GuiAtlas::subregion_quad`, because `geometry` maps the *whole* sprite through
its `GuiScaling` and would have stretched all 256x256 into the 147x166 rect.

The page is registered as a **loose extra** on the HUD's atlas
(`resources::RECIPE_BOOK_TEXTURES`, id `recipe_book/panel` — a name vanilla does
not use, so it can never collide with a real sprite, which matters because
`build_with_extras` silently skips an extra whose id is already claimed).

Three things to know before changing this:

- **The flat fills are still there, and should stay.** They are the jar-less
  picture, every existing headless geometry gate measures them, and the opaque
  page hides them completely when an atlas is bound.
- **List order is draw order.** The page must be first (it is opaque and would
  erase anything before it) and the toggle **last** (the panel is clamped and may
  overlap the main panel's left edge, so a page drawn over the toggle buries a
  live control).
- **Two documented deviations.** Slot frames are emitted for *populated* cells
  only, matching vanilla hiding an unused `RecipeButton` — the sheet's grid
  region is uniform white with no frames baked in, so drawing all 20 would add a
  grid vanilla lacks. And the filter button always uses the crafting art, never
  `furnace_filter_disabled`: the geometry function is not given the `Menu`, and
  threading one in changes a caller outside this module. Craftability is not
  modelled either, so `slot_craftable` is used unconditionally — this panel
  browses the whole corpus rather than only what the inventory can make, so
  greying everything out would be the more misleading of the two.

### Stack counts must be submitted after the icons they sit on

There is **no depth compare** on this GUI path, so submission order alone decides
z. The panel originally emitted a slot well and then that cell's icon, per cell,
in one interleaved loop, and drew the whole colour stream in a single pass before
the item passes — so every count digit went down before its own icon and
vanished. The owner reported it as "the item counts are behind the items (at
least the blocks)", and the "at least" is the diagnostic: a flat item sprite is
transparent around its edges so digits bled through, whereas a 3-D block model
fills the bottom-right corner opaquely and hid them outright.

Fixed the way `ContainerGeometry` already did it: the geometry emits all chrome
first, records `chrome_vertex_count`, then the icons, and the draw is four
passes — chrome, art, models, then sprites plus the icon-overlay colour range.
This was **recipe-panel only**; the main container and inventory path was
already correct.

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
but `hud.rs` **does** render it now (`HudFrame::recipe_toast`, see "Shell
wiring" below), so the toast appears the moment a producer exists.

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

The plan is a `Vec<PlacementStep>` (`{cell: menu_slot, source_slot:
menu_slot}`), not a network action. `app.rs`'s `auto_fill_clicks` turns one
into clicks, and **the click sequence is not "two per step"**, which is what
this section used to say and is wrong in a way that reads correct:

`plan_auto_fill` emits one step per grid cell, each moving a **single** item,
and several steps can name the same `source_slot` (one coal stack supplying
three cells). `Click::left` on a slot places the **whole** carried stack
(`click.rs`: "pick up whole / place whole"), so a literal "pick up from
`source_slot`, place into `cell`" pair puts a 5-coal stack entirely in the
first cell and leaves every later cell empty. That is not a hypothetical: it
is the executed negative control
`two_clicks_per_step_would_dump_the_whole_stack_in_one_cell` in `app.rs`,
which observes exactly `Some(("minecraft:coal", 5))` in cell 0.

The sequence that actually yields one item per cell is vanilla's own manual
gesture, **grouped by source slot** (first appearance, not adjacency — steps
are ordered by cell, so one source's cells need not be consecutive):

1. `Click::left(source_slot)` — the whole stack onto the cursor;
2. `Click::right(cell)` for each cell that source supplies — "place one";
3. `Click::left(source_slot)` — return the remainder (a no-op when the source
   was exhausted exactly, so it needs no guard).

Every click goes through `WindowApp::send_menu_click`, i.e. the same
per-click predict-then-send path a manual `MenuInput::press`/`release` takes.
A second dispatch path would diverge from `container.rs`'s vanilla-exact
click semantics.

Sending vanilla's own `PlaceRecipe` action instead is still not an option:
its wire field is a **`RecipeDisplayId`**, a server-session-assigned integer
from the undecoded `recipe_book_add` packet, not this corpus's `Identifier` —
so this client cannot construct a correct one without that same decode.

### Shell wiring — `app.rs`, `hud.rs` (landed)

The three brokered shell patches are **applied**, so the panel, the auto-fill
and the toast all reach pixels. What each one is:

- **Persisted panel state** — `app.rs`'s `RecipePanelState` (`open`, `search`,
  `tab`, `page`, `search_focused`), a field on `WindowApp` beside
  `recipe_book`. Persisted across container open/close on purpose: vanilla's
  own book state lives on the client's `RecipeBook`, not on the screen, so
  reopening a crafting table keeps the book open on the same tab.
- **Hit-testing** — `WindowApp::handle_recipe_panel_click` runs *before* the
  existing `hit_test_with_scale` in the `WindowEvent::MouseInput` arm, and
  returns whether it consumed the click. Order matters: the panel overlaps the
  main panel's left edge at narrow canvases (the clamp above), so testing it
  second would make its own widgets unclickable there. Only a **press** is
  offered to it — a release must still reach `MenuInput::release` so a drag
  that began on a real slot can terminate.
- **The draw pass** — `HudRenderer::render_recipe_book_panel`, called from
  `redraw` immediately after `ContainerRenderer::render_with_icons_scaled`.
  It lives on `HudRenderer` rather than `ContainerRenderer` because the
  colour/sprite/model pipelines a `RecipeBookPanelGeometry` needs already
  exist there and `ContainerRenderer` exposes no entry point taking a prebuilt
  geometry. The streams are byte-compatible by construction, not coincidence:
  the panel's colour verts come from the same shared `item_icon::ColourStream`
  the HUD's do (6 floats, NDC), its item verts from the same
  `push_sprite_quad` (8 floats).
- **The toast** — `HudFrame::recipe_toast` / `hud.rs`'s `RecipeToastView` and
  `draw_recipe_toast`, fed each frame by `recipe_toast_view`.

**Two gotchas worth knowing before changing any of this.**

`recipe_panel_layout` is called by *both* the hit-test and the draw, and must
stay that way — `container.rs`'s own `hit_test_with_scale` warns that a layout
built with a different `gui_scale` than the frame was drawn with silently
mis-resolves every click, and one shared function is the only structural
guarantee they agree. For the same reason `hud.rs` exposes
`recipe_toast_rect`, so the toast's draw and any gate measuring it share one
expression rather than restating `canvas_w - 160.0`.

The panel geometry keeps **one unsplit colour stream**, unlike
`ContainerGeometry` with its `chrome_vertex_count` split. So the draw order is
colour → sprites → models, and a recipe result's stack-count digits (emitted
into that same colour stream by `draw_stack`) land *under* its icon rather
than over it. Splitting the stream belongs in `container.rs`; it affects only
multi-output recipes' count text.

#### Toast geometry, read from the record

Every number is from the **definitions** in `.cache/mc/26.2/client-src`, not a
summary of a call site — this repo has a documented instance of transcribing a
Java record's positional fields backwards:

- `Toast.width() == 160`, `Toast.height() == 32` (`Toast.java:39-45`).
- `xPos(screenWidth, visiblePortion) == screenWidth - width() *
  visiblePortion` (`Toast.java:31-33`). This is **not** a fixed right margin:
  it is the slide-in. At `visiblePortion == 1.0` the left edge sits exactly
  160 from the right edge.
- `yPos(firstSlotIndex) == firstSlotIndex * height()` (`Toast.java:35-37`), so
  the first toast is **flush with the top of the screen at `y == 0`**, not
  inset by a margin. We draw at most one, so `firstSlotIndex == 0`.
- Contents (`RecipeToast.extractRenderState`, `:55-65`): background sprite
  `toast/recipe` over the full `160×32` (the sprite really is in 26.2's atlas,
  checked against the jar); title at `(30, 7)` colour `-11534256` =
  `0xFF500050`; description at `(30, 18)` colour `-16777216` = black; the
  station icon at `(3, 3)` under a `scale(0.6)` that scales the **position
  too**, so it lands at `(1.8, 1.8)` at `9.6px`; the unlocked item at
  `(8, 8)`, unscaled.
- Strings are the real ones from `en_us.json`: `"New Recipe(s) Unlocked!"` and
  `"Check your recipe book"` — note the parenthesised plural a paraphrase
  loses.

Vanilla's 600ms slide (`ToastManager.java:229-232`) is **not** modelled:
`RecipeToastQueue` exposes no animation origin (`last_changed_ms` is private
and it has no notion of a visibility transition), so `visible_portion` is
fixed at `1.0`. The field exists and the draw honours it, so whoever gives the
queue a real producer can add the slide without touching the geometry.

`sim.rs` needed **no** change, despite the brokered list naming it: the
dispatch seam `WindowApp::send_menu_click` already goes straight to
`ClientHandle::menu_click` and deliberately bypasses `Sim`/`NetClient`, as its
own doc comment records.

### Still brokered

1. **Protocol decode** (`crates/protocol/v770/src/adapter.rs`): clientbound
   arms for `place_ghost_recipe`/`recipe_book_add`/`recipe_book_remove`/
   `recipe_book_settings`/`update_recipes`, plus a `ClientEvent` variant in
   `lodestone-model` to decode into and a `net.rs`/ingest consumer that calls
   `RecipeUnlockState::unlock`/`remove` and `RecipeToastQueue::push`.

Until that lands, two things degrade **honestly and visibly**: the panel shows
the full local corpus (`RecipeUnlockState::is_unlocked` reports everything
unlocked until `has_data()` flips), and the toast never fires, because nothing
can call `push`. No fake producer was added to make either light up early —
that would be the island defect one layer down. `app.rs`'s
`recipe_toast_now_ms` is the clock a future producer should push on, so the two
sides cannot pick incompatible origins.

Tracked on [#436](https://github.com/matteopolak/lodestone/issues/436).

### Gates

The wiring has its own gates, because `container.rs`'s 75 geometry tests all
passed while the panel drew nothing — a crate's own suite is a closed loop.

In `app.rs` (`mod recipe_book_wiring`): every vertex the draw submits lands
inside the `[-1, 1]` NDC clip range (the sweep that catches the whole
"geometry exists, nothing on screen" class, and the one that found the
author's own two bugs — tabs at `bx - 30` going off-canvas and a
`Builder::new(1.0, 1.0, None)` placeholder), the same sweep at a canvas narrow
enough to hit the `RECIPE_PANEL_MIN_X` clamp, and coverage of the panel's own
screen rect derived from `recipe_panel_layout` itself. Auto-fill asserts the
**resulting slot contents** after the dispatch loop, on a torch — chosen
because its arithmetic is falsifiable: a `1×2` shape in a `3×3` grid occupies
cells `0` and `3`, since the stride is the *grid's* width, and a hand-count
using the shape's width predicts `0` and `1`.

In `hud.rs` (`mod recipe_toast_gate`): the toast covers `Toast.java`'s own
rect, and `the_toast_is_anchored_to_the_top_right_corner` predicts the value
rather than asserting a sign — at half visibility the left edge must be
`cw - 80`, which a fixed-margin transcription (still reporting `cw - 160`)
fails.

Each has an **executed** control. A closed panel must cover *none* of the book
rect while still emitting its toggle (so the control is not vacuous for the
other reason: nothing drawn at all), and
`no_toast_frame_paints_nothing_in_the_toast_rect` **verifies the control's
premise** instead of assuming it — a control here once failed at 3.5% because
the first-person bare arm was painting, a premise false since long before the
feature existed. Failure output prints a **bounding box**, not just a
fraction, because a fraction cannot tell a uniform-but-wrong frame from a
localised blob.

## Dependencies

- `lodestone-model` — `Identifier`, `ClientEvent`, `ItemStack`, `Text`.
- `serde_json` (optional, feature `json`).
- Consumed by `lodestone-client` (`state.rs` owns a `Menus`), which the shell
  reads through `ClientHandle::player_menu` / `open_menu`.
