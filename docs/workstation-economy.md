# The workstation economy: anvil, grindstone, smithing table, enchanting table

## What it is

The server-side maths and click wiring for the four cost-driven container
screens issues #253–#255 ask for: the anvil (repair-with-material,
repair-by-combining, rename, the prior-work penalty, the too-expensive cap),
the grindstone (strip enchantments to curses, combine-repair, a partial XP
refund), the smithing table (netherite upgrade, armour/tool trim) and the
enchanting table (bookshelf power, the three-slot level cost, weighted-random
offers). `docs/container-cost-screens.md` already covers the **client** half
(menu shape, layout, background art, cost-number rendering) — this is the
half that was still missing: nothing server-side computed a result, charged
XP, or consumed an input.

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
  `Grindstone`, `Smithing`) selects three things that differ per station:
  `may_place` on the input cells (`item_combiner_may_place`), the quick-move
  ranges, and — the one genuinely bespoke piece — how a take consumes the
  input cells (`take_result`'s three-way match: crafting/smithing shrink
  every cell by one, the grindstone always fully clears both cells, and the
  anvil clears cell 0 unconditionally while cell 1 depends on
  `repair_item_count_cost`/`only_renaming`, re-derived from
  `crate::anvil::compute` with `creative: true` purely to read those two
  fields — safe because creative can only ever *widen* which combination
  produces a result).
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
- A fifth `Station`-shaped menu: a `Station` (or a wholly new `MenuKind`
  variant, if it has no result slot like `Enchanting`) in `container_click.rs`,
  a `PlayerInventory::open_workstation`-style opener in `server.rs`, and a
  pure compute module alongside `anvil.rs`/`smithing.rs`/`enchanting.rs`.

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
cargo test -p lodestone-server --lib --no-fail-fast -- enchantment_data:: anvil:: smithing:: enchanting:: container_click::
cargo test -p lodestone-server --lib --no-fail-fast -- server::tests::the_anvil_repairs server::tests::the_grindstone_strips server::tests::the_smithing_table_upgrades
cargo test -p lodestone-server --lib --no-fail-fast -- server::tests::rename_item server::tests::container_button_click server::tests::a_pure_rename_take
cargo test -p lodestone-v770 --no-fail-fast -- serverbound_interaction_tier2::
```
