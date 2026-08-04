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

## The declared island: nothing in production calls any of these yet

**State plainly: this is the server-side simulation half only.** All four types
compile, are unit-tested, and are exported — and **nothing outside their own
test modules constructs or ticks one.** This is a disclosed island, not an
oversight; CLAUDE.md's rule 1 explicitly permits landing one when the missing
hop is named. Here it is, precisely:

1. **No tick loop drives them.** `crates/lodestone-server/src/integrated.rs`'s
   `IntegratedServer::open_in_memory_with_mobs` (`:162-210`) spawns a second
   task, `mobs::run_mob_tick_loop`, that owns a `MobSim` and ticks it every
   50 ms — this is the exact, directly-copyable pattern a
   `run_block_entity_tick_loop` would follow. It was not added here: a
   `BlockEntityRegistry` keyed by world position needs somewhere to live
   (there is no `HashMap<BlockPos, T>` anywhere in this crate's chunk model —
   `chunk.rs`'s `ChunkColumn` stores only a block-state palette, confirmed by
   reading it directly), and building one *without* a real consumer able to
   populate it (see the next point) would be exactly the "looks wired but
   isn't" trap CLAUDE.md warns is worse than an honest declared gap — a hopper
   ticked forever with `above: None, below: None` passed in produces zero
   observable behaviour, ever, and a registry that only ever does that is not
   meaningfully more wired than no registry at all.
2. **No decoded packet can create one.** `crates/lodestone-server/src/server.rs`'s
   `apply_use_item_on` (`:550-576`) always places `minecraft:stone` regardless
   of the item held — its own doc comment already says so ("this crate has no
   inventory model to resolve a real item from"). Even though `PlayerInventory`
   (issue #408, `18c682d`) now exists and *could* answer "what item is the
   player holding", `apply_use_item_on` was not updated to consult it — that is
   a real gap but a different one from this issue's scope (it would let a
   furnace/composter block exist in the world at all; still says nothing about
   opening or ticking one).
3. **No client screen threads cost/progress data.** `container_set_data` is
   already decoded into `Menus::container_data` (`crates/lodestone-game/src/menus.rs`)
   and unit-tested against a hand-built furnace menu — but nothing in
   `crates/lodestone-shell/src/sim.rs` reads it, and `server_protocol.rs` has
   **no** encoder for `CONTAINER_SET_DATA`/`OPEN_SCREEN`/`CONTAINER_SET_CONTENT`/
   `CONTAINER_SET_SLOT` at all (checked directly: zero hits in
   `crates/protocol/v770/src/server_protocol.rs`). This half is explicitly
   **not this issue's** — `sim.rs` is brokered, and the task that dispatched
   this work named it as another agent's follow-up.
4. **The brewing stand's `Bottle` is not a real `ItemStack`.**
   `lodestone_model::ItemStack`'s `ItemComponents` has no potion-contents
   field (checked directly: `custom_name`/`damage`/`enchantments`/
   `dyed_color`/`tool`/`max_stack_size`/`max_damage`/`equippable`/
   `has_unmodeled`, nothing potion-shaped) — `brewing.rs` uses its own small
   `Bottle { kind, potion }` type instead of expanding the shared model, which
   is out of this issue's file ownership.

**What *would* close each hop**, for whoever picks this up next:

- A `HashMap<BlockPos, BlockEntityState>` (an enum over the four types here,
  or four separate maps) added to `IntegratedServer` or a new sibling struct,
  ticked from a `run_block_entity_tick_loop` spawned the same way
  `run_mob_tick_loop` is.
- `apply_use_item_on` consulting `PlayerInventory::selected_item` and, when it
  names a block-entity block (`minecraft:furnace`, `minecraft:composter`,
  `minecraft:hopper`, `minecraft:brewing_stand`), inserting a fresh entry into
  that registry at the placement position instead of always writing stone.
- `V770ServerProtocol` encoders for the four container packets named above,
  and `sim.rs` reading `container_data` into whatever draws the furnace flame/
  arrow (tracked separately, per the dispatching brief).
- Hopper adjacency specifically needs a way to ask "what's the container (if
  any) at world position P" — the registry above, keyed by position, answers
  this for free once it exists; `hopper::try_move_one_item` and
  `Hopper::tick` already take arbitrary `&mut [Option<ItemStack>]` slices, so
  no change is needed there, only a caller that resolves the two adjacent
  positions into slices before calling `tick`.

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
work, not done here — and there is nothing in a running game to point one at
yet regardless (see the declared-island section above), since no served
world can construct one of these block entities today.
