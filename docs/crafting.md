# Crafting

## What it is

The version-free crafting stack in `lodestone-game`: the recipe data model and
matching rules (`recipe.rs`), a loader for Mojang's own datapack JSON
(`recipe_json.rs`), the crafting-table menu layout, the plugin-facing
recipe-registration API, and the recipe-book UI (browsing, auto-fill, unlock
toast) layered on top. Our own server now computes crafting results too — see
[Server-authoritative gameplay](./server-gameplay.md).

## How it works

### Who computes the result slot

**The server does.** Vanilla's own crafting-menu slot-change hook runs server-side
and pushes the result as a `container_set_slot` for slot 0; the vanilla
client never matches recipes itself. This client does the same, so a local
recipe corpus is **not** on the critical path for "put items in, see the
result" — it exists for the recipe-book UI, a ghost preview before the
round trip lands, and offline use.

Since 1.21.2 `update_recipes` no longer carries the crafting corpus itself —
only recipe *property sets* and the stonecutter list — and the recipe
**book** arrives separately via `recipe_book_add` as display-only entries.
Neither substitutes for the datapack corpus.

### Data model and matching

`Ingredient` (item id / tag / `Any` of several), a memoised cycle-guarded
`TagResolver`, `CraftingGrid` (a row-major id snapshot), and `Recipe` (shaped,
shapeless, cooking, stonecutting, two smithing kinds, transmute, and
hard-coded `Special` recipes). Two matching rules that are easy to get
subtly wrong: a shaped pattern matches at **any offset** and, by default,
**mirrored** left-to-right, with every cell the pattern does not cover
required to be empty (this is what stops a solid 3×3 of planks from being a
chest); shapeless matching is a **bipartite perfect matching**, not "each
ingredient appears somewhere" — the naive version lets one item satisfy two
ingredient slots.

`CorpusBuilder` is source-agnostic (`push_recipe`/`push_tag` take
`(Identifier, &str)` pairs) and records a malformed document in `failures()`
rather than aborting the load, so one unknown recipe type from a future
version cannot leave the client with none. `load_data_root` walks a
datapack's `data/` root **recursively** — a flat `read_dir` silently drops a
third of 26.2's item tags, since tag ids are path-derived
(`tags/item/enchantable/weapon.json` → `minecraft:enchantable/weapon`).

Slot order is the trap in the menus: window 0 is `0` result / `1..=4` craft /
`5..=8` armour / `9..=35` main / `36..=44` hotbar / `45` off-hand, while a
crafting table is `0` result / `1..=9` grid / `10..=36` main / `37..=45`
hotbar — **no armour, no off-hand, hotbar not at 36**. `MenuKind` stays
`Generic` for a crafting table (positionally it is one); branch on
`craft_layout()`, never on `MenuKind`, to know whether a menu crafts. Slot
*kinds* are load-bearing too — a plain slot at index 0 lets a shift-click
deposit into the result slot and desyncs every later prediction.

### Runtime recipe registration (the plugin API)

`RecipeBook::register`/`unregister` are the validated counterparts to the
JSON loader's `insert`: a plugin gets `Err(Duplicate)` on a colliding id and
`Err(ReservedNamespace)` on a `minecraft:`-namespaced one, where the JSON
loader silently replaces and allows either. `lodestone_ecs::recipes::RecipeRegistry`
is the shared resource plugin `Plugin::build` calls into — since that runs
long before `client.jar`'s corpus loads, registrations are held **pending**
and replayed onto whichever corpus the process later adopts, making
registration order-independent. The shell re-clones the merged book only
when `RecipeRegistry::revision` has moved, so a mid-session registration
reaches the screen at the cost of one `u64` comparison per frame rather than
cloning a 1,585-recipe corpus every frame.

Gotchas: `TagResolver`'s memo must be an `RwLock`, not a `RefCell` — `RefCell`
is `!Sync`, which propagates through `RecipeBook`, which cannot then be a
`bevy_ecs` `Resource` at all; `unregister` must go through `RecipeBook`,
never a caller-side `Vec::remove`, because a stale `grid_index` entry does
not panic, it silently degrades an unrelated recipe's own matching. Server-side
plugin registration is not wired: the host's bundled recipe corpus is
independent of the plugin registry, so a registered recipe is client-side
prediction and recipe-book UI only against a real server.

### Recipe-book UI

Browsing (`RecipeBook::browse`) substring-matches the **result item's id**,
not a resolved display-name search tree the way vanilla's real client does —
a deliberate simplification, since this client has no resolved-name index to
build a fuzzy search from. Categories/tabs are read from each recipe JSON's
own `"category"` field and vanilla's own per-book tab list (declaration
order, not alphabetical, and not symmetric — a blast furnace has no Food tab,
a smoker has *only* Food).

Auto-fill (`Recipe::placement` + `plan_auto_fill`) is the **inverse** of
`match_grid`: given a recipe, which ingredient goes in which cell (always
top-left, never mirrored — vanilla's own placement position is not decoded
here). The click sequence that actually places one item per cell is **not**
"pick up, place, per step" — `plan_auto_fill` emits one step per grid cell and
several steps can share a source slot, so a literal per-step pick-up/place
pair dumps a whole stack into the first cell. The real sequence, grouped by
source slot: pick up the whole stack, right-click ("place one") into each
cell that source supplies, then pick the whole stack up again to return any
remainder.

The unlock toast (`RecipeToastQueue`) merges multiple unlocks within a 5-second
window into one **cycling** toast rather than stacking separate ones. The
first sync of a session seeds the "already toasted" set from the whole known
list without toasting any of it — vanilla does not replay a fresh join's
entire unlock history as toasts, only genuinely new ones after that. Toast
geometry (position, colours, station/result icon placement and scale) is
transcribed from vanilla's `Toast`/`RecipeToast` records directly, not
inferred from a call site — this repo has a documented case of a Java
record's positional fields being transcribed backwards.

Recipe-unlock tracking has two separate stores and this is the thing to get
right: `RecipeUnlockState` (keyed on `Identifier`) is genuinely dead — the
wire's `RecipeDisplayId` is a session-assigned integer with no `Identifier`
on it at all, so nothing can ever feed this store, and it permanently reports
every recipe unlocked as an honest stand-in. The real per-session tracking is
`lodestone_game::recipe_sync::RecipeBookSync`, keyed on the wire's own
`RecipeDisplayId` directly — do not read `RecipeUnlockState`'s continued
dormancy as evidence the feature itself is unfinished.

The server folds inbound recipe-book open/filter changes into the connection's
`PlayerInventory::recipe_book_settings` state. This state is separate from the
crafting grid and is intentionally session-scoped until player-data persistence
has an authoritative recipe-book representation.

The host also owns the per-connection "new" state for its recipe display ids.
Every entry in the initial `recipe_book_add` snapshot is highlighted without a
toast; when a visible recipe button makes the client send
`recipe_book_seen_recipe`, the server validates that opaque id against the same
book it advertised, clears only that entry's highlight, and returns a
non-replacing one-entry update so the client read-model clears too. The ids and
the acknowledgements are session-local, so they deliberately do not enter
player persistence.

## How to change it

- Validation rules for plugin registration belong in
  `RecipeRegistration::validate`, not the ECS layer (transport only).
- Adding a matcher feature: extend `recipe.rs`'s `Recipe`/`match_grid`, and
  check whether the recipe-book auto-fill (`Recipe::placement`) needs the
  same inverse operation added.
- The crafting menu's slot-order table above is restated, not shared, between
  a generic container and a crafting table — check both when adding a new
  screen kind.

## Configuration

Cargo feature `json` on `lodestone-game` (off by default) enables
`recipe_json`; the shell enables it explicitly
(`lodestone-game = { workspace = true, features = ["json"] }"`) to load the
real corpus from `client.jar` at GPU bring-up. Corpus tests read
`.cache/mc/26.2/client-src/data` (gitignored) and are `#[ignore]`d.

## Dependencies

`lodestone-model` (`Identifier`, `ClientEvent`, `ItemStack`, `Text`);
`serde_json` (optional, feature `json`); `bevy_ecs` for the registration
resource. Nothing here is version-specific — recipes are `Identifier`-keyed,
never numeric ids, so no protocol family is involved. Consumed by
`lodestone-client` (`Menus`) and `lodestone-shell` (`container.rs`,
`resources.rs::load_recipe_book`, `app.rs`/`hud.rs` for the panel and toast).
