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
