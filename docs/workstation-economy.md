# The workstation economy: anvil, grindstone, smithing table, enchanting table, loom, stonecutter

## What it is

The server-side maths and click wiring for the cost-driven container screens
issues #253–#255 (and, for the loom/stonecutter, #150) ask for: the anvil
(repair-with-material, repair-by-combining, rename, the prior-work penalty,
the too-expensive cap), the grindstone (strip enchantments to curses,
combine-repair, a partial XP refund), the smithing table (netherite upgrade,
armour/tool trim), the enchanting table (bookshelf power, the three-slot
level cost, weighted-random offers), the loom (banner pattern application)
and the stonecutter (its own recipe list). `docs/container-cost-screens.md`
already covers the **client** half (menu shape, layout, background art,
cost-number rendering) — this is the half that was still missing: nothing
server-side computed a result, charged XP, or consumed an input.

The loom and stonecutter are the two stations with **no** cost at all —
unlike the other four, taking a result never charges or refunds XP, so they
are documented here for the shared `Station`/`ItemCombiner` machinery, not
for any economy of their own.

## How it works

### The pure maths: one module per station, one shared registry

- [`crates/lodestone-server/src/enchantment_data.rs`](../crates/lodestone-server/src/enchantment_data.rs)
  — the vanilla `minecraft:enchantment` registry (43 entries: weight,
  max level, the linear min/max cost curve, anvil fee, the seven exclusive
  sets, curse/treasure membership) plus the 77-item `minecraft:enchantable`
  census, transcribed from the 26.2 jar's own data (`.cache/mc/26.2/src/data/minecraft/enchantment/*.json`
  and the generated component reports), not from a wiki. Every other module
  reads this one table rather than carrying its own copy.
- [`crates/lodestone-server/src/anvil.rs`](../crates/lodestone-server/src/anvil.rs)
  — `AnvilMenu.createResult` ported field for field (`compute`), plus the
  grindstone's `computeResult`/`mergeItems`/`removeNonCursesFrom`/XP-refund
  formula (`grindstone_result`, `grindstone_xp`). Both stations live in one
  file because the grindstone's combine-repair is the anvil's own durability
  formula with the enchant-merge swapped for a strip.
- [`crates/lodestone-server/src/smithing.rs`](../crates/lodestone-server/src/smithing.rs)
  — netherite upgrade (`netherite_upgrade`, all 12 `diamond_<x>` →
  `netherite_<x>` recipes, uniform enough to be one data table) and armour
  trim (`apply_trim`, the 18 patterns × 11 materials), tried as two
  independent recipe families — a template can never satisfy both.
- [`crates/lodestone-server/src/enchanting.rs`](../crates/lodestone-server/src/enchanting.rs)
  — `bookshelf_power` (the real 32-position ring geometry, not "count nearby
  bookshelves"), `cost_for_slot`/`table_costs` (`EnchantmentHelper.getEnchantmentCost`)
  and `select_enchantments` (`EnchantmentHelper.selectEnchantment`'s weighted
  draw-and-filter loop).
- [`crates/lodestone-server/src/loom.rs`](../crates/lodestone-server/src/loom.rs)
  — `result`: applies one new [`lodestone_model::BannerPatternLayer`] to a
  banner, from either a specific pattern *item* (10 items, each mapped to its
  one granted pattern — `tags/banner_pattern/pattern_item/*.json`, transcribed
  as `(item, pattern)` pairs because the mapping is **not** the identity for
  two of the ten) or the 32-pattern base grid
  (`tags/banner_pattern/no_item_required.json`, in file order — not a verified
  live-registry iteration order). A pattern item's single option auto-selects
  (`LoomMenu.slotsChanged`'s own `size() == 1` branch), so the common case
  needs no `ContainerButtonClick` at all.
- [`crates/lodestone-server/src/stonecutting.rs`](../crates/lodestone-server/src/stonecutting.rs)
  — `matches`/`result`: filters `crate::crafting::recipe_book()`'s own
  `Recipe::Stonecutting` entries (see below — these were **not** bundled
  before issue #150) by ingredient, sorted by recipe id for a stable
  (disclosed non-vanilla-exact) button order.

Every non-trivial formula's own doc comment cites the vanilla method and
file it was read from; see each module for the derivation rather than
duplicating it here.

### Click wiring: `MenuKind::ItemCombiner` and `MenuKind::Enchanting`

None of the four stations is a [`crate::block_entities::BlockEntity`] in
vanilla — each menu's input slots are scratch space the menu itself owns and
throws away on close (`AnvilMenu.inputSlots`, `GrindstoneMenu.repairSlots`,
`SmithingMenu.inputSlots`, `EnchantmentMenu.enchantSlots`), the same
"positionless virtual menu" shape `crates/lodestone-server/src/crafting.rs`'s
crafting-table support already established. `PlayerInventory::workstation`
(`crates/lodestone-server/src/inventory.rs`) is that scratch space here: a
flat `Vec<Option<ItemStack>>` sized to the open station, opened by
`open_workstation`/`open_enchanting_screen`-adjacent code in `server.rs` and
cleared back to the player on `ServerBound::ContainerClosed` — the same
"do not silently delete items on close" rule `take_table_crafting` already
follows.

`crates/lodestone-server/src/container_click.rs` gained two new `MenuKind`
variants:

- `ItemCombiner { inputs, station: Station }` — `inputs` grid cells then one
  take-only result, exactly `ItemCombinerMenu`'s own
  `getInventorySlotStart() == resultSlot + 1`. `Station` (`Anvil`,
  `Grindstone`, `Smithing`, `Loom`, `Stonecutter`) selects three things that
  differ per station: `may_place` on the input cells
  (`item_combiner_may_place`), the quick-move ranges, and — the one
  genuinely bespoke piece — how a take consumes the input cells
  (`take_result`'s match: crafting/smithing/stonecutter shrink every cell by
  one — the stonecutter's single input cell falls into that same default
  arm, needing no dedicated one — the grindstone always fully clears both
  cells, the loom clears/shrinks only cells 0/1 (banner, dye) and
  deliberately leaves cell 2 (the pattern item) untouched so it can stamp a
  second banner, and the anvil clears cell 0 unconditionally while cell 1
  depends on `repair_item_count_cost`/`only_renaming`, re-derived from
  `crate::anvil::compute` with `creative: true` purely to read those two
  fields — safe because creative can only ever *widen* which combination
  produces a result).
  - `Loom`'s three cells are banner/dye/pattern-item, not a fourth-shape —
    `MenuLayout::item_combiner`'s own `inputs` match groups it with
    `Smithing` by cell *count* only; `item_combiner_may_place` still
    dispatches per station.
  - `Stonecutter` has exactly one input cell.
  - Both feed a second piece of per-connection scratch state beyond the
    grid: `PlayerInventory::selected_recipe_index` (`LoomMenu.selectedBannerPatternIndex`/
    `StonecutterMenu.selectedRecipeIndex`'s own `DataSlot`), reset to `None`
    by `open_workstation` exactly like `pending_rename`/`enchant_seed`. It is
    threaded into `workstation_result`'s `selected` parameter (ignored by
    the other three stations, the same "unused by most, real for one or two"
    shape `rename` already had), so the two closures that recompute the
    result mid-click (`read_workstation_menu`, `apply_workstation_clicked`'s
    own `recipe` closure) both see it.
- `Enchanting` — two cells (item, lapis), **no result slot at all**: the item
  is enchanted in place, so there is nothing to take.

`server.rs`'s `apply_container_clicked` dispatches both variants to dedicated
functions (`apply_workstation_clicked`, `apply_enchanting_clicked`) before
falling into its existing `CraftingTable`/`Container` branches, because
neither new shape's grid source is a `crate::crafting::CraftingState` and
forcing it through `read_menu` would silently drop every placed item (no
block entity exists at the station's position to write "own" slots into).

XP is charged **outside** `container_click`/`apply_container_clicked` —
both are deliberately economy-free, matching their own module docs — in the
`ServerBound::ContainerClicked` handler in `server.rs`, which detects a
result-slot take (the clicked menu slot equals the station's result index)
and charges/awards `PlayerExperience` from the **pre-click** cells before
`apply_container_clicked` overwrites `PlayerInventory::workstation`. The
enchanting table's own XP/lapis charge is a third, sibling site rather than a
fourth station folded into that same handler: `MenuKind::Enchanting` has no
result slot to take at all (see above), so its economy is charged by
`ServerBound::ContainerButtonClick`'s own consumer,
`crate::server::apply_container_button_click`, on a successful enchant
rather than on a take.

## How to change it

- A new anvil/enchant formula constant: edit `anvil.rs`/`enchanting.rs`
  directly and re-derive from `.cache/mc/26.2/src`, never from memory — see
  `CLAUDE.md`'s own warning about this exact family of maths.
- A new enchantment, or a balance change to an existing one:
  `enchantment_data.rs`'s `ENCHANTMENTS` table, `EXCLUSIVE_SETS`, and (if it
  introduces a new `supported_items` tag) a new `SupportedItems` variant.
- A sixth `Station`-shaped menu: a `Station` (or a wholly new `MenuKind`
  variant, if it has no result slot like `Enchanting`) in `container_click.rs`,
  a block-name entry in `apply_use_item_on`'s dispatch table and a
  `workstation_menu_type`/`container_title` entry in `server.rs`, and a pure
  compute module alongside `anvil.rs`/`smithing.rs`/`enchanting.rs`/
  `loom.rs`/`stonecutting.rs`. Note `open_workstation_screen` itself needed
  **no** change for the loom/stonecutter — it was already generic over
  `Station`, `inputs`, and the menu-type string.
- A new pattern item for the loom: one row in `loom.rs`'s `PATTERN_ITEMS`,
  keyed by the *pattern* id its own tag file grants (not by guessing the
  item's own name — see that table's own doc for the two rows where they
  differ). A new stonecutting recipe needs **no** change anywhere in this
  crate: it loads through `crate::crafting::recipe_book` automatically once
  its JSON lands in `assets/recipe/` (see below).

### Known gaps, and why they are gaps

- **Enchantment identity has no real registry.** `ServerProtocol::encode_registry_data`
  (`crates/protocol/v770`) sends only `minecraft:dimension_type` and
  `minecraft:world_clock` during Configuration — never `minecraft:enchantment`
  — so `lodestone_model::ItemEnchantment::id` has no synced registry for a
  real client to resolve against. `enchantment_data::id_of`/`name_of` assign
  a stable **internal-only** id (alphabetical over the 43-entry table) so
  this crate's own logic stays self-consistent; the enchantment glint still
  renders (it is a bare "is the list non-empty" check), but a real client
  cannot show the enchantment's *name* until `encode_registry_data` grows a
  `minecraft:enchantment` entry — a `crates/protocol/v770` change outside
  this crate's ownership.
- **Closed: the enchanting table's offer can now be taken.**
  `ServerboundContainerButtonClickPacket` (`ClientAction::ContainerButtonClick`)
  used to decode-and-discard in `crates/protocol/v770/src/server_protocol.rs`;
  it now lifts into `ServerBound::ContainerButtonClick { window_id, button_id }`
  and reaches `crate::server::apply_container_button_click`. That function
  re-derives the slot's cost from the *live* world (`enchanting::bookshelf_power`
  read at click time, not cached), re-derives the offer from
  `enchanting::select_enchantments` seeded off
  `PlayerInventory::enchant_seed`, and on a successful pick: applies the
  enchantment(s) (transmuting a plain book to `minecraft:enchanted_book` the
  same way vanilla does), spends XP levels (`PlayerExperience::take_levels`),
  consumes lapis, and rerolls the seed — `Player.onEnchantmentPerformed`'s own
  reroll, so the next offer set is not a repeat. The three `container_set_data`
  costs sent at open time still reflect the (empty) menu at that instant and
  are still not live-recomputed as the item slot changes (`slotsChanged`'s
  own recompute) — a real client's displayed numbers can lag what a click
  actually prices until the *next* full resend, which is a display gap, not a
  gameplay one: the button's own re-derivation is what is trusted.
  `PlayerInventory::enchant_seed` is seeded once at open
  (`open_enchanting_screen`'s own `enchant_seed_roll` parameter, a pre-drawn
  value from the connection's `SpawnRng` — the same shape
  `apply_use_item_on`'s composter `roll` already uses) rather than from a
  persistent per-player field vanilla keeps in `Player`, which this crate has
  no equivalent of; the practical difference is invisible past the first
  table open in a session.
- **Closed: the anvil's rename field is reachable.**
  `ServerboundRenameItemPacket` (`ClientAction::RenameItem`) used to decode-
  and-discard the same way; it now lifts into `ServerBound::RenameItem { name }`
  and reaches `crate::server::apply_rename_item`, gated on an anvil actually
  being open (mirrors `ServerGamePacketListenerImpl.handleRenameItem`'s own
  `containerMenu instanceof AnvilMenu` check). The text is filtered and
  length-capped by `crate::anvil::validate_rename` (`AnvilMenu.validateName`,
  ported field for field: drops control characters, `DEL` and the `§` prefix,
  then **rejects** — never truncates — anything left over 50 characters), then
  stored as `PlayerInventory::pending_rename` and threaded into every
  subsequent `crate::anvil::compute` call for that menu, so the "type a name,
  see the 1-XP rename cost" path — the one thing `docs/container-cost-screens.md`
  already had a client-side reader waiting for — now actually reaches pixels.
  One genuinely bespoke case needed a second fix: `container_click::take_result`'s
  own internal re-derivation of the anvil outcome is deliberately rename-free
  (that module carries no economy state at all), so a take priced *purely* by
  a pending rename saw a zero price internally and would have wrongly cleared
  a present-but-not-consumed addition cell as if a real combine had happened.
  `apply_workstation_clicked` corrects this after the fact, with the real
  rename text available — a no-op unless a click actually took such a result.
- **The anvil block's own 12% degrade chance is not modelled** — it needs
  block-state writes (`chipped_anvil`/`damaged_anvil`) this module has no
  `ChunkSource` access to. Cosmetic only; the repair/combine economy itself
  is unaffected.
- **Closed: the loom and stonecutter had zero server-side menu support at
  all** (issue #150) — not merely an unconnected `ContainerButtonClick`, as
  an earlier pass on this issue had assumed. There was no `Station` variant,
  no block-open dispatch, and (the part that took the longest to find) the
  stonecutter's whole recipe corpus was **not bundled**: `assets/recipe/`
  held only `crafting_shaped`/`crafting_shapeless` (1,056 files,
  `crate::crafting::BUNDLED_CRAFTING_RECIPES`'s old value), so
  `recipe_book()` had zero `Recipe::Stonecutting` entries in production —
  `crate::stonecutting::matches` returned an empty list for every input
  until the 319 real `stonecutting` recipe JSON files were copied in
  alongside them (`BUNDLED_CRAFTING_RECIPES` is now 1,375; `build.rs`
  needed no change, since it already bundles the whole directory generically
  by file, keyed only by the JSON's own `"type"`). Both are now real,
  server-computed, and gated through the production `apply_container_clicked`
  → `apply_workstation_clicked` → `container_click::take_result` path (see
  `server::tests::a_stonecutter_button_click_then_take_produces_the_selected_recipe_and_consumes_one_input`/
  `server::tests::a_loom_take_with_a_pattern_item_consumes_banner_and_dye_but_not_the_pattern_item`).
- **Not modelled: exact vanilla button ordering for either station's offer
  list.** The loom's 32-pattern base grid is the tag file's own listed
  order (a disclosed transcription, not a verified live-registry iteration
  order); the stonecutter's offer list is sorted by recipe id, which is
  stable across calls but not proven to match `RecipeManager`'s real
  registration order. Getting either exactly right would need a JVM oracle
  dump the same way `EntityDataIndexOracle` exists for metadata indices —
  nobody has built one for `Registries.BANNER_PATTERN`/stonecutting
  registration order yet. Harmless for the auto-select common case (a
  specific pattern item, or a stonecutting input with only one recipe);
  visible only as "button N might not be vanilla's button N" for a
  multi-option `ContainerButtonClick`.

## Configuration

None — no flags or env vars gate this.

## Dependencies

`crates/lodestone-model` (`ItemStack`/`ItemComponents::repair_cost`, added
alongside this work — server-side bookkeeping only, not carried over the
wire; see that field's own doc), `lodestone_data::item_prototypes` (damage/
stack-size/equip-slot census), `crate::container_click`, `crate::inventory`,
`crate::experience::PlayerExperience`, `crate::mob_spawn::SpawnRng` (the
grindstone's XP-bonus roll and the enchanting table's offer draw — same
documented non-JVM-bit-compatible RNG `crate::loot` already uses; draw
*order and count* match vanilla, the underlying bit stream does not).

## Verification

```bash
cargo test -p lodestone-server --lib --no-fail-fast -- enchantment_data:: anvil:: smithing:: enchanting:: loom:: stonecutting:: container_click:: crafting::
cargo test -p lodestone-server --lib --no-fail-fast -- server::tests::the_anvil_repairs server::tests::the_grindstone_strips server::tests::the_smithing_table_upgrades
cargo test -p lodestone-server --lib --no-fail-fast -- server::tests::rename_item server::tests::container_button_click server::tests::a_pure_rename_take
cargo test -p lodestone-server --lib --no-fail-fast -- server::tests::a_stonecutter_button_click_then_take server::tests::a_loom_take_with_a_pattern_item
cargo test -p lodestone-v770 --no-fail-fast -- serverbound_interaction_tier2::
```
