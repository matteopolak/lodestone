# Block-entity simulation: composter, furnace, hopper, brewing stand

Issues [#249](https://github.com/matteopolak/lodestone/issues/249) (composter),
[#251](https://github.com/matteopolak/lodestone/issues/251) (furnace),
[#250](https://github.com/matteopolak/lodestone/issues/250) (hopper),
[#252](https://github.com/matteopolak/lodestone/issues/252) (brewing stand).

## What it is

Four independent, pure, tick-driven state machines in `crates/lodestone-server/src/`,
each a line-by-line port of the corresponding vanilla `BlockEntity`/`Block` class:

| Module | Type | Vanilla source | Commit |
|---|---|---|---|
| `composter.rs` | `Composter` | `ComposterBlock.java` | `fb23564` |
| `furnace.rs` | `Furnace` | `AbstractFurnaceBlockEntity.java` + `FuelValues.java` | `fb23564` |
| `hopper.rs` | `Hopper` | `HopperBlockEntity.java` | `7f75055` |
| `brewing.rs` | `BrewingStand` | `BrewingStandBlockEntity.java` + `PotionBrewing.java` | `c85fefa` |

All four are exported from `lodestone-server`'s `lib.rs`. Each module's own doc
comment is the primary reference — file:line citations, the exact control flow
quoted from the decompiled source, and what is deliberately not modeled. This
doc is the index and the wiring-gap record; read the module for the mechanic
itself.

## How it works

Each type is a **plain value type with a `tick(&mut self) -> …Tick` method** —
no `ChunkSource`, no connection, no async — the same shape
[`PlayerVitals`](../crates/lodestone-server/src/vitals.rs) and
[`FallTracker`](../crates/lodestone-server/src/fall.rs) already established for
this crate's other tick-driven mechanics. A `…Tick` result struct reports what
changed (lit/cooked/brewed/ejected/…) so a future caller knows which client-bound
packet, if any, would be worth sending.

Numbers were sourced from `.cache/mc/26.2/src/` (decompiled 26.2 source,
authoritative) and, for the furnace's 181-entry smelting/blasting/smoking
recipe table, directly from Mojang's own generated recipe JSON under
`.cache/mc/26.2/client-src/data/minecraft/recipe/` — not memory, not a wiki.
See each module's doc comment for the exact citations.

Every test predicts an **exact tick**, not "eventually" (CLAUDE.md's magnitude
rule): a furnace's `iron_ore_smelts_into_one_ingot_at_exactly_tick_200`, a
composter's `reaches_ready_at_exactly_tick_20_after_hitting_level_seven`, a
hopper's `next_transfer_attempt_is_exactly_8_ticks_after_a_success`, a brewing
stand's `brew_completes_at_exactly_tick_400_and_consumes_one_ingredient`. Every
negative claim ("no fuel means no cooking", "a disabled hopper never
transfers", "swapping the ingredient mid-brew aborts it") has a control that is
run and observed to actually exercise the gate, per CLAUDE.md's evidence
standard.

## Update: gaps 1 and 2 are closed — a furnace now exists and ticks in a real world

**`crates/lodestone-server/src/block_entities.rs`** (new) closes the first two
of the three gaps named below:

1. **The registry now exists.** [`BlockEntityRegistry`] is a
   `HashMap<BlockPos, BlockEntity>` (`BlockEntity` an enum over the four
   types), and [`BlockEntityHandle`] is the `Arc<Mutex<_>>`-backed, `Clone`
   handle that lets a connection's own task (placement) and a background
   tick-loop task (advancing every entry) share one registry — the exact
   shape `crate::mobs::LiveMobSource` already established for the mob
   simulation. `BlockEntityRegistry::tick_all` advances every entry once.
   **Update (issue #284):** the async driver is now `tick::run_tick_loop` —
   a single unified 20Hz loop that ticks block entities *and* the mob sim
   together, with MSPT/TPS/overrun accounting (issue #285; see
   [`docs/server-tick-loop.md`](server-tick-loop.md)). Before #284 this was
   `block_entities::run_block_entity_tick_loop`, a second loop spawned
   side-by-side with the mob tick task (`mob_task`/`block_entity_task` were
   separate sibling fields on the handle); that function still exists and is
   still covered by its own test, but is no longer what
   [`IntegratedServer::open_in_memory_with_mobs`] spawns — this is still the
   constructor a real singleplayer session uses (`lodestone-shell/src/net.rs`),
   so a furnace placed there really does tick every 50ms, not just in a test.
2. **Placement now honours the held item, for these four blocks.**
   `crate::server::apply_use_item_on` now consults
   `PlayerInventory::selected_item()` through `block_entity_for_item`: a
   furnace/smoker/blast-furnace/composter/hopper/brewing-stand item writes
   its own block (still bare, no `facing`/`lit` properties — no per-block
   orientation model exists, unchanged from before) and inserts a fresh
   `BlockEntity` into the registry at the placement position; anything else,
   including an empty hand, still falls back to `minecraft:stone` exactly as
   before. This is a narrow, additive extension of `docs/block-edit.md`'s
   existing scope cut, not a general item→block registry.

**Gap 3 (container packets) is still open** — see its own section below,
unchanged from the original write-up. Closing 1 and 2 already makes the
following true: `crates/protocol/v770/tests/block_entities_live.rs`'s
`real_client_places_a_furnace_and_the_server_registers_it` drives a **real**
`lodestone-client` through a `CONTAINER_CLICK` (a furnace lands in hotbar
slot 0, native slot 0 via menu slot 36) then `USE_ITEM_ON`, and asserts both
that the
wire confirms a real `minecraft:furnace` block (not the old always-stone
fallback) at the target cell, and that the server's own `BlockEntityRegistry`
holds a real `BlockEntity::Furnace` there afterward — with a negative control
(an empty hand still places stone and registers nothing) run and observed to
pass. `block_entities.rs`'s own `run_block_entity_tick_loop_actually_advances_a_registered_furnace_over_time`
(`#[tokio::test(start_paused = true)]`, the same precedent
`tests/serve_play.rs` set) proves the *background async loop* — not just a
synchronous `tick_all` call — really ticks a furnace: loaded with coal and
iron ore, it is lit after the very first real tick the loop performs, and
holds a real `minecraft:iron_ingot` in its output slot by 10 (virtual)
seconds — predicting the exact values `furnace.rs`'s own
`iron_ore_smelts_into_one_ingot_at_exactly_tick_200` test already pins, not
just that "something changed."

**Gap 3 (container packets) is now closed too — see the update section
below.** The paragraph that used to be here said nobody could see inside a
placed furnace; that is no longer true for a furnace or a hopper.

**Two scope cuts worth being explicit about**, found while wiring the tick
loop:

* **Hopper adjacency only resolves another hopper.** `BlockEntity::hopper_slots_mut`
  answers `None` for every non-hopper variant — a hopper next to a furnace or
  composter never transfers anything, the same "no `Container` seam over the
  furnace's three separate slots" gap the original write-up predicted below.
  Two hoppers stacked *do* transfer, and — proven by
  `tick_all_moves_two_items_between_a_stacked_hopper_pair_on_the_first_tick`,
  run and its outcome checked rather than assumed — they move **two** items
  on the first tick, not one: each hopper's own tick independently attempts
  both a push and a pull, so a stacked pair gets vanilla's real "double
  hopper" 2x throughput for free, an artifact of ticking each entity once
  rather than a bug.
* **No visual sync.** A lit furnace or a ready composter does not change
  what a client is streamed — ticking mutates the registry only, never
  `ChunkSource::set_block`. `block_entities.rs`'s own module doc comment
  states this as a real, separate gap.

## Update: gap 3 is closed too — a real client can open and load a furnace

**`server_protocol.rs` now has all four container-packet encoders**
(`OPEN_SCREEN`, `CONTAINER_SET_CONTENT`, `CONTAINER_SET_SLOT`,
`CONTAINER_SET_DATA`), and `crate::server` has both the "open" side and the
"click reaches the block entity" side:

- **Right-clicking a block that already has an openable [`BlockEntity`]
  opens its screen instead of attempting a placement.**
  `apply_use_item_on` now checks `block_entities.get(pos).and_then(BlockEntity::menu_name)`
  *before* any placement logic, mirroring vanilla's own order
  (`ServerGamePacketListenerImpl.handleUseItemOn` runs the clicked block's
  own `useItemOn` before any `BlockPlaceContext`). `open_container_screen`
  then sends `open_screen`, a full `container_set_content` (the entity's own
  [`BlockEntity::container_slots`] followed by the player's standard 27-main
  + 9-hotbar rows, per vanilla's `addStandardInventorySlots`), and every
  `container_set_data` property (`BlockEntity::data_properties`) — the exact
  sequence vanilla's `ServerPlayer::openMenu`/`initMenu` produce.
- **`BlockEntity::menu_name` answers for two of the four kinds, `None` for
  the other two, for two different reasons**: [`Composter`] has no vanilla
  menu at all (`ComposterBlock` never opens a screen — it is a per-click
  add/empty against the block entity directly), and [`BrewingStand`] has a
  real menu but this crate cannot open it, because its `Bottle` slots are
  not real `ItemStack`s (the second named gap just below, unchanged). Only
  [`Furnace`]/[`Hopper`] can be opened today.
- **A `container_click` against the open window reaches the block entity.**
  `apply_container_clicked` now splits a non-zero window's menu-slot index
  via `crate::inventory::container_menu_slot` into "the block entity's own
  slot" (written through `BlockEntity::set_container_slot`) or "the player's
  standard inventory tail" (written into `PlayerInventory` exactly as
  before) — closing the last punch-list item below ("a way to *load* a
  furnace... with fuel/ingredients from a client's own inventory").
- **A furnace's own background tick reaches an open window with no client
  click at all** — `docs/block-entities.md`'s own design question, quoted in
  the issue that asked for this. `crate::server::sync_open_container` diffs
  the block entity's current slots/data against what was last pushed to
  this connection, on its own 50ms timer (`CONTAINER_SYNC_INTERVAL`,
  matching the background tick loop's own cadence) — so a furnace that
  ignites or finishes smelting is reflected to an open client even though
  nothing it did caused that change.
- **Breaking a block now removes its entity from the registry**, closing a
  dangling-entry risk that only became real once a screen could stay open
  against one (`apply_block_action`'s `StopDestroy` arm).

**Proof, not just wiring**: `crates/protocol/v770/tests/block_entities_live.rs`'s
`real_client_opens_a_placed_furnace_and_loads_it_via_container_click` drives
a real `lodestone-client` through the existing placement flow, then a second
`UseItemOn` at the *same* position (opening it — asserted via the client's
own `open_menu()` reporting `menu_type == minecraft:furnace` and a 39-slot
menu), then a real `CONTAINER_CLICK` placing iron ore and coal into the
furnace's own slots 0/1 — polling the **server's own registry** (not the
wire) for the load to land, per this repo's evidence standard. A sibling
test, `opening_a_furnace_does_not_reset_its_already_loaded_contents`, is the
negative control: a furnace pre-loaded before the connection even starts
must still report its real contents on open, not a blank freshly-placed one.
Watched failing first with the "check for an existing menu" branch
neutered (`existing_menu` forced to `None`) — the open timed out — then
restored via `cp` from a scratchpad backup with an md5 check, per this
repo's shared-checkout convention.

`crates/protocol/v770/tests/container_encoders.rs` is the hermetic half: all
four encoders round-tripped through the **real** `V770Adapter::handle_packet`
(not a bespoke reader), including a negative control (an unknown menu key
must emit nothing) and a clear-a-slot control.

**What is still genuinely open**, stated plainly:

1. **The brewing stand's `Bottle` is not a real `ItemStack`** — unchanged
   from the original write-up. `lodestone_model::ItemStack`'s
   `ItemComponents` has no potion-contents field, so a brewing stand cannot
   be opened until either a real potion-contents component exists or a
   second, bottle-shaped wire path is built. Out of this landing's file
   ownership either way.
2. **The composter has no menu to open, by design** — matching vanilla,
   which never opens a screen for one either. Right-clicking a composter
   with an item still falls through to this crate's existing plain-block
   placement logic (unchanged): there is no serverbound handling of "feed
   the composter one item," which is composter-specific future work, not a
   container-packet gap.
3. **This crate does not send a `container_close` to force a client's UI
   shut** when the block backing an open window is broken out from under
   it (`apply_block_action` clears the *server's* own `OpenContainer`
   tracking and the registry entry, but the client's screen would only
   notice via a future interaction). A real, documented gap, not attempted
   here — vanilla's own equivalent is `AbstractContainerMenu::stillValid`
   polling every server tick, which this crate does not model.
4. **`sim.rs`'s furnace flame/progress-arrow rendering** is `lodestone-shell`
   territory (off-limits to this session) — `Menus::opened_data()` and
   `draw_anvil_cost`/`draw_enchanting_costs` (issue `3b2bcc5`) already
   consume `container_set_data` generically by property index, so a correct
   index here (verified against `AbstractFurnaceMenu.java`/
   `AbstractFurnaceBlockEntity.java`'s own `ContainerData` table) is what
   makes that rendering possible once someone extends it to furnaces — not
   done by this landing.
5. **No server-authoritative `doClick`** for a non-zero window either,
   consistent with `docs/server-inventory.md`'s existing scope note for
   window `0`: this crate applies the click's own predicted diff verbatim
   rather than re-deriving vanilla's seven click modes server-side.

## How to change it

- Each mechanic's numbers live entirely inside its own module (`composter.rs`'s
  `compostable_chance`, `furnace.rs`'s `cooking_recipe`/`base_burn_duration`,
  `hopper.rs`'s `TRANSFER_COOLDOWN_TICKS`, `brewing.rs`'s `potion_mix`). Adding
  a missing recipe/fuel/compostable/potion-mix entry is a one-line addition to
  the relevant match, cited against the same vanilla source the rest of the
  table came from — do not add an entry without a `file:line` (or JSON
  filename) citation.
- The furnace's recipe table was generated mechanically from the recipe JSON
  corpus rather than hand-typed (see `furnace.rs`'s module doc comment for the
  exact source directory); if that corpus changes (a new MC version), regenerate
  rather than hand-editing the 181-arm match.
- None of the four modules depend on each other or on `chunk.rs`/`worldgen_data.rs`
  (both off-limits to this work) — safe to extend in isolation.

## Configuration

None — no feature flags or env vars gate any of these four modules.

## Dependencies

- `lodestone_model::ItemStack` — composter/furnace/hopper's item representation
  (matches `crate::inventory::PlayerInventory`'s own choice, see
  `docs/server-inventory.md`).
- `crate::furnace`'s recipe table has no dependency on `lodestone-game`'s own
  `recipe.rs`/`recipe_json.rs` (a deliberate restatement, not a shared
  dependency — this crate stays client-free, the same reasoning
  `docs/server-inventory.md` gives for restating `lodestone-game`'s menu
  slot table rather than importing it).

## Verification

```bash
cargo test -p lodestone-server --lib --no-fail-fast -- composter::
cargo test -p lodestone-server --lib --no-fail-fast -- furnace::
cargo test -p lodestone-server --lib --no-fail-fast -- hopper::
cargo test -p lodestone-server --lib --no-fail-fast -- brewing::
cargo test -p lodestone-server --lib --no-fail-fast -- block_entities::
cargo test -p lodestone-server --lib --no-fail-fast -- server::
cargo test -p lodestone-server --lib --no-fail-fast -- inventory::
cargo test -p lodestone-data --lib --no-fail-fast -- menus::
cargo test -p lodestone-v770 --test block_entities_live
cargo test -p lodestone-v770 --test container_encoders
cargo test -p lodestone-v770 --test server_inventory_live
cargo check -p lodestone-server -p lodestone-v770 -p lodestone-data --all-targets
```

**No live-oracle gate exists for any of the four, and this session did not run
any RCON commands against the live oracles.** Stated plainly rather than
implied: the numbers above (burn/cook/brew times, fuel durations, fill
chances, potion mixes) come from `.cache/mc/26.2/src/` — decompiled 26.2
source, data source #2 per `CLAUDE.md`, real and de-obfuscated for this
version — and, for the furnace's recipe table, directly from Mojang's own
generated recipe JSON (data source #1, authoritative). These are compiled-in
Java constants and static data files, not server-configurable state, so
`/data get block` on a real furnace would confirm NBT field *names*
(`BurnTime`, `CookTime`, …) but not the tick-count constants themselves,
which live in code the decompile already exposes directly. Cross-checking
against a live oracle remains valuable *evidence-independence* per
CLAUDE.md's evidence standard (two sources, one code-derived and one
behavioural, agreeing is stronger than either alone) and is real follow-up
work, not done here. Note this paragraph is stale in one respect, kept
deliberately rather than silently rewritten: a served world can now
construct a *loadable* block entity via placement and a real container
click (the update section above), so "gap 3 still blocks loading fuel or an
ingredient into one" is no longer why an RCON cross-check has not been run —
it simply has not been run, and would mostly test placement/registry/click
plumbing rather than the tick-count constants this paragraph is actually
about.
