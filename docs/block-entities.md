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
   simulation. `BlockEntityRegistry::tick_all` advances every entry once;
   `block_entities::run_block_entity_tick_loop` is the async driver
   [`IntegratedServer::open_in_memory_with_mobs`] now spawns alongside the
   existing mob tick task (`mob_task`/`block_entity_task` are siblings on
   the handle, joined/aborted together) — this is the constructor a real
   singleplayer session uses (`lodestone-shell/src/net.rs`), so a furnace
   placed there really does tick every 50ms, not just in a test.
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

**What still isn't possible without gap 3**: nobody can *see* inside a placed
furnace — no client screen, no progress arrow, no way to load fuel/ingredient
into one at all (the registry can only ever hold what a placement inserts:
an empty block entity). So today's furnace ticks forever empty unless a test
reaches into the registry directly (as the async-loop test above does) —
real, observable ticking in a shipped session still waits on gap 3.

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

## The remaining named gap: no client can see inside one yet

Gaps 1 ("no tick loop drives them") and 2 ("no decoded packet can create
one") from the original write-up are closed — see the update section above.
What is still open:

1. **No client screen threads cost/progress data.** `container_set_data` is
   already decoded into `Menus::container_data` (`crates/lodestone-game/src/menus.rs`)
   and unit-tested against a hand-built furnace menu — but nothing in
   `crates/lodestone-shell/src/sim.rs` reads it, and `server_protocol.rs` has
   **no** encoder for `CONTAINER_SET_DATA`/`OPEN_SCREEN`/`CONTAINER_SET_CONTENT`/
   `CONTAINER_SET_SLOT` at all (checked directly: zero hits in
   `crates/protocol/v770/src/server_protocol.rs`, still true as of this
   update). This half is explicitly **not this landing's** either — `sim.rs`
   is brokered, and another agent was reported to be wiring
   `Menus::container_data`'s consumer concurrently with this session's work.
2. **The brewing stand's `Bottle` is not a real `ItemStack`.**
   `lodestone_model::ItemStack`'s `ItemComponents` has no potion-contents
   field (checked directly: `custom_name`/`damage`/`enchantments`/
   `dyed_color`/`tool`/`max_stack_size`/`max_damage`/`equippable`/
   `has_unmodeled`, nothing potion-shaped) — `brewing.rs` uses its own small
   `Bottle { kind, potion }` type instead of expanding the shared model, which
   is out of this issue's file ownership.

**What would close gap 3**, for whoever picks this up next:

- `V770ServerProtocol` encoders for the four container packets named above
  (`OPEN_SCREEN`, `CONTAINER_SET_CONTENT`, `CONTAINER_SET_SLOT`,
  `CONTAINER_SET_DATA`) — `crates/protocol/v770/src/server_protocol.rs` still
  has zero hits for any of the four.
- A `ServerBound` decode + `crate::server` consumer for whatever serverbound
  packet opens a window against a block entity (today `apply_container_clicked`
  only ever applies against `window_id == 0`, the player's own inventory —
  see `docs/server-inventory.md`'s own "a new window" note, which this
  landing does not change).
- `sim.rs` (brokered) reading `container_data` into whatever draws the
  furnace flame/arrow.
- Once a window can be opened, a way to *load* a furnace/composter/hopper/
  brewing-stand with fuel/ingredients from a client's own inventory — today
  the registry can only ever hold what `apply_use_item_on` inserts at
  placement time (an empty block entity), since nothing else can reach into
  one yet.

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
cargo test -p lodestone-v770 --test block_entities_live
cargo test -p lodestone-v770 --test server_inventory_live
cargo check -p lodestone-server -p lodestone-v770 --all-targets
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
work, not done here. Note this is now stale in one respect: a served world
*can* construct a (real but permanently empty, per the update section above)
block entity today via placement, so "no server crate" is no longer why an
RCON cross-check has not been run — it simply has not been run, and would
mostly test placement/registry plumbing rather than the tick-count constants
this paragraph is actually about, since gap 3 still blocks loading fuel or an
ingredient into one from a real client.
