# Block drops

## What it is

Breaking a block server-side now rolls that block's vanilla loot table, spawns the result as real item
entities with vanilla's `popResource` position and velocity, streams them to every connection, and lets a
player walk over them to collect them into their inventory. This is issue #337's *consumer* — the loot
roller itself already existed and had never been called.

## The five links, and which were already there

#337 was closed once and reopened by the closed-issue audit as a "confirmed island". The audit was right,
but the island was smaller than it looked: most of the chain existed, in pieces that had never been joined.

| link | state before this work |
|---|---|
| 1. a block breaks | **existed** — `apply_block_action`'s `StopDestroy` arm |
| 2. its loot table is rolled | **built, zero production callers** — `loot.rs`, 1,551 lines, 6 bundled tables |
| 3. an item entity is spawned | **existed, one caller** — `MobSim::spawn_item`, reached only by the composter's bone meal |
| 4. it is broadcast | **existed but carried the wrong value** — see below |
| 5. it falls / merges / is picked up | falls: **existed**. merges: built, zero callers. pickup: **absent** |

So the missing pieces were: the call from 1 to 2, the block→table-id mapping, `popResource`'s geometry, the
merge driver, all of pickup — and a **shipped bug** in link 4.

## The acacia-boat bug

`MobSim::snapshots` set each dropped item's `EntitySnapshot::entity_type` to **the item's own key**:

```rust
entity_type: state.item.clone(),   // e.g. `minecraft:bone_meal`
```

`entity_type` is an *entity* type. `minecraft:bone_meal` is not in the entity-type registry, so `v770`'s
`encode_add_entity_body` resolved it through `entity_type_id(name).unwrap_or(0)` — and network entity type
`0` is `minecraft:acacia_boat`. **Every dropped item this server has ever spawned arrived at the client as
a boat**, with no error logged anywhere.

Two things are worth keeping about how this survived:

* `cargo xtask connectedness` could not see it. The wire was fully connected; the value on it was wrong.
  That is the same shape as #323's `SET_TIME` (every link green, wall-clock time instead of the tick
  counter), and the tool is documented as blind to it.
* **A test asserted the bug as correct.** `projectile_and_item_registries.rs` had
  `assert_eq!(item_snap.entity_type, rk("minecraft:diamond"))`, under a test named
  "…_with_their_own_identity_…". The name and the assertion agreed with each other, so nothing about it
  read as wrong on inspection.

The item's *identity* belongs in metadata (`ItemEntity.DATA_ITEM`), not in this field.

## How it works

`apply_block_action`'s `StopDestroy` arm, in order:

1. reads the block state **before** `set_block(AIR)` — vanilla's `Level.destroyBlock` captures it first for
   the same reason, and after the write it is unrecoverable;
2. `block_drops::drop_block_loot` maps the state to `minecraft:blocks/<path>` (`Block.getLootTable`'s
   default), looks it up in the process-wide bundled set, rolls it against the empty `LootContext`, and
   turns each stack into a `PoppedItem` carrying `popResource`'s position and velocity;
3. each `PoppedItem` goes to `MobSim::spawn_item` with `ItemLifecycle::newly_dropped`, whose 10-tick delay is
   `setDefaultPickUpDelay()`.

From there the existing machinery takes over with no new task: `tick::run_tick_loop` already ticks `MobSim`
every 50 ms (advancing lifecycle and `ItemMotion` fall dynamics), and `MobSim::snapshots` already feeds the
per-connection `EntityStreamer` diff, which is the "broadcast path" — a *pull*, so a new item is picked up by
every connection's next streaming pass with nothing pushed. (#438's claim that there is no broadcast path and
no player registry is **stale**: `players.rs` landed both.)

Pickup runs in `serve_play`, right after the player's position is republished and *before* the streaming
pass, so the item's `REMOVE_ENTITIES` and the inventory's `container_set_slot` go out together.

### `popResource`'s draw order is the specification

From `Block.java:412-419` plus the `ItemEntity` constructor at `ItemEntity.java:61-66`, **five draws in the
order x, y, z, vx, vz**. `vy` is the constant `0.2` and consumes no draw. A port that draws for `vy`, or that
draws velocity before position, produces a statistically identical cloud of items and desyncs from vanilla
per-seed — which is why `pop_resource_placement` owns the RNG rather than accepting an offset, and why one
test recomputes all five values from a parallel stream instead of only checking the envelope.

`EntityTypes.ITEM` is `.sized(0.25F, 0.25F)`, so the `- halfHeight` term is `0.125`.

### Two volumes, neither of them a radius

* **Pickup** is `Player.aiStep`'s `getBoundingBox().inflate(1.0, 0.5, 1.0)` intersected against the item's
  own box. Both sets of half-extents contribute: horizontal reach is `0.3 + 1.0 + 0.125 = 1.425`, and the
  vertical band is asymmetric (`0.5 + 0.25` below the feet, `1.8 + 0.5` above). A sphere of radius 1.0 is
  wrong three separate ways.
* **Merging** is `mergeWithNeighbours`' `inflate(0.5, 0.0, 0.5)`. The **`0.0`** is load-bearing: stacks side
  by side merge, stacks a block apart vertically never do. One isotropic radius would merge a drop with one
  sitting on the block below.

### Pickup destination order

`Inventory.getSlotWithRemainingSpace` searches **selected hotbar slot → off-hand (40) → natives 0..36**, and
only merges into non-empty slots; `getFreeSlot` then places a fresh stack scanning **`items` (0..36) only**.
So a *merge* into the off-hand is normal and a *fresh stack* there is impossible. "First empty slot" still
puts the item in the inventory, which is why the gate asserts the destination rather than the arrival.

`PlayerInventory::add` returns every native index it wrote, because a stack that overflows one slot must
produce two `container_set_slot`s. `window_zero_menu_slot` converts those to menu coordinates and is tested
as the exact inverse of the existing menu→native table rather than by restating it.

## How to change it, and the gotchas

* **Do not add a table by hand.** `assets/loot_table/` is now generated: `just regen-loot-corpus` copies every
  clean table out of `.cache/mc/26.2/client-src`, and its drift gate compares the tree against the *cache*.
  Adding one file by hand fails that gate. To bring more tables in, teach `loot.rs` the feature they use and
  regenerate — see `docs/loot-tables.md`.
* **1,230 of vanilla's 1,355 tables are bundled** (#538), including 1,035 block tables. The 125 excluded ones
  use a feature the roller does not model, and a block among them still drops nothing — honest rather than
  guessed, since a block with no table returns an empty `Vec` exactly as one whose table rolled nothing does.
  Blocks vanilla itself gives no table (`bedrock`, `barrier`, the fluids, `end_portal`) take the same path
  correctly.
* **`doTileDrops` is not honoured.** Vanilla wraps `popResource` in
  `level.getGameRules().get(GameRules.BLOCK_DROPS)`. This crate has no live game-rule registry to consult —
  `game_rules.rs` describes rules for the wire and stores no values. A real registry is the prerequisite.
* **The tool is two separate mechanisms, and conflating them is the trap** (#539). Silk Touch, Fortune and
  `match_tool` are *loot* features and live in `loot.rs` against `LootContext::tool`, which
  `drop_block_loot` fills from its `held` argument. The **correct-tool** requirement is not a loot condition
  at all — see the section below.
* **The RNG stream is not vanilla's.** Draw *count* and *order* are; the stream is SplitMix64 vs vanilla's
  Xoroshiro, and vanilla additionally splits position draws (level RNG) from velocity draws (entity RNG).
  Byte-exact JVM parity is separate, larger work — `loot.rs`'s module doc records the same divergence.
* **Partial pickup shrinks the entity rather than deleting it**, matching vanilla's in-place `ItemStack`
  shrink. `MobSim::set_item_count` does this as a remove-and-respawn at the same id (preserving `age`, so a
  full player brushing past cannot reset the despawn clock), because `ItemEntityRegistry` in
  `lodestone-entity` exposes no count setter.

## The correct-tool gate is not a loot condition (#539)

`block_drops::drops_are_allowed` is vanilla's `Player.hasCorrectToolForDrops`
(`Player.java:617-619`): `!state.requiresCorrectToolForDrops() || selectedItem.isCorrectToolForDrops(state)`.
`ServerPlayerGameMode.destroyBlock` (`:295`) consults it and, when it is false, **never calls `playerDestroy`
→ `dropResources` at all**. So a bare hand on stone breaks the block and yields nothing; before #539 it
dropped a cobblestone.

It must stay outside the roll, and folding it in would be wrong twice:

1. the roll's RNG draws would still happen, shifting the stream for the *next* break on that connection;
2. a table with no `match_tool` branch at all would still be consulted.

The computation itself was already in the tree — `lodestone_data::tool::mining(held, state_id).correct_tool`
is *this* flag, already folded with the block's own `requiresCorrectToolForDrops`, and the two are routinely
confused (`lodestone-shell`'s `sim.rs` carries the same warning for the mining-speed divider). The only new
plumbing is `mobs::block_state_id`, the name→state-id bridge every id-keyed census needs from a world that
stores canonical strings.

**A stone-only fixture cannot test this**, which is why `serve_play.rs` grew a `StoneWithDirtSource`: every
block in the stone fixture *requires* a correct tool, so a gate mis-implemented as "you need a tool" passes
every stone assertion. Dirt requires none, and the same bare hand must still drop it.

## How the drop becomes *visible* — `ItemEntity.DATA_ITEM` (#537)

A client draws nothing for an item entity whose stack it has not been told: vanilla's
`ItemEntityRenderer.submit` returns early on `state.item.isEmpty()`, and this project's client does the same.
For one landing this server sent `EntitySnapshot::metadata: Vec::new()` for every drop, so a broken block
spawned a real item entity that fell, merged and could be picked up — the pickup *visibly*, because the
inventory slot updates — while drawing zero pixels, with every link on the path green.

The stack travels as `ItemEntity.DATA_ITEM`, carried by `MetadataField::Item { item, count }`:

1. `protocol.rs` defines the variant. It carries a `ResourceKey`, which is not `Copy`, so **`MetadataField`
   no longer derives `Copy`** — deliberately, and permanently: a version-free vocabulary enum that derives
   `Copy` silently forbids every future field with an owned value, and the whole cost of dropping it is that
   an implementor writes `match field` rather than `match *field`.
2. `crates/protocol/v770/src/server_protocol.rs` encodes it: index **8**, serializer **7** (`ITEM_STACK`),
   then `write_optional_item_stack`'s VarInt count / VarInt registry id / empty `DataComponentPatch`. There is
   still **no `_ =>` arm** in `encode_set_entity_data`, on purpose — a new field must be encoded or fail to
   compile.
3. `MobSim::snapshots`' item loop fills `metadata` from `ItemState::item` plus the lifecycle count. The
   client's existing chain (`Value::Item` decode → `DisplayItem` → `resolve_entity_facts` → `EntityDraw::item`
   → `world_items.rs`) needed no change; it was already live and gated by
   `crates/lodestone-shell/tests/live_dropped_item.rs`.

### Where the index came from, and the collision that is *not* inheritable

Both numbers were read off the `EntityDataIndexOracle` dump already in the tree
(`crates/protocol/v770/tests/support/entity_data_index_jvm.txt:55` — `8 ItemEntity.DATA_ITEM 7 ITEM_STACK`),
never hand-counted, and the same two bytes appear in a packet captured off a real vanilla 26.2 server
(`crates/protocol/v770/tests/fixtures/item_entity_metadata_diamond.hex`).

Index 8 is the most contended index in the dump — **nineteen** claimants, including
`LivingEntity.DATA_LIVING_ENTITY_FLAGS`, `AbstractArrow.ID_FLAGS`, `ExperienceOrb.DATA_VALUE`,
`PrimedTnt.DATA_FUSE_ID`, and six other `ITEM_STACK` fields. **Neither `entity_census::is_living` (index 8's
living-vs-arrow precedent) nor `is_mob` (index 15's mob-vs-armour-stand precedent) separates the claimants
here**: both report *false* for `minecraft:item`, which does not distinguish it from `AbstractArrow` or
`PrimedTnt`. The encoder needs no census column because the guard is structural — the field list is built by
`snapshots`' **item** loop, so every `MetadataField::Item` belongs to a `minecraft:item` entity by
construction. **The invariant to keep is on the producer: never push a `MetadataField::Item` from the mob or
projectile loops.**

## Gates and their controls

`crates/lodestone-server/tests/serve_play.rs` drives the real `serve_connection` path over a solid-stone
world. Every control below was run and observed failing:

| gate | control | observed failure |
|---|---|---|
| breaking stone drops one cobblestone item, streaming as `minecraft:item` | revert `entity_type` to the item key | `left: "minecraft:cobblestone", right: "minecraft:item"` |
| an aborted dig drops nothing | drop on `AbortDestroy` too | `left: 1, right: 0` |
| a collected drop leaves the world and announces menu slot 36 | — (positive) | — |
| a freshly popped drop is not collectable | ignore `can_be_picked_up` | `left: 0, right: 1` |
| a drop 10 blocks away is not collected | make `is_within_pickup_range` return `true` | `left: 0, right: 1` |
| the drop's snapshot carries `Item { cobblestone, 1 }` | revert `metadata` to `Vec::new()` | `left: [], right: [Item { … "cobblestone" …, count: 1 }]` |
| bare-handed stone drops nothing, bare-handed dirt still drops | make `drops_are_allowed` return `true` | `a bare hand on stone is hasCorrectToolForDrops == false … left: 1, right: 0` |
| an unenchanted tool does not satisfy the silk-touch predicate | make `match_tool` tool-presence-only | `left: "minecraft:stone", right: "minecraft:cobblestone"` (plus five other assertions) |
| `ore_drops` draws nothing at level 0 | use the issue body's `count * max(1, nextInt(level + 2))` | `left: Vec3 { x: 0.3820…}, right: Vec3 { x: 0.4504… }` — **and nothing else failed**, which is the point |
| `ore_drops`' support is `1..=level+1` | drop the `- 1` / `max(0)` clamp | `fortune 1 produced coal x3; count * (max(nextInt(3), 1)) cannot exceed 2` |

`crates/protocol/v770/tests/server_item_entity_metadata.rs` gates the wire half against the **captured
vanilla packet**, so the expected value predates the encoder: our metadata list must be byte-identical to
`08 07 01 9e 07 00 00 ff`. Control — drop the `DataComponentPatch` from the item write (the single most
plausible transcription error): `ours = [08, 07, 01, 9e, 07, ff], vanilla = [08, 07, 01, 9e, 07, 00, 00, ff]`,
and the same neuter also made the real client adapter raise no `EntityMetadataUpdated` at all.

The fixture is stone rather than "a block" deliberately: stone's table exercises `alternatives` +
`match_tool`, so the fall-through to cobblestone is proved rather than "an item dropped". `block_drops.rs`'s
unit tests cover all five bundled tables, which between them add `survives_explosion`, `table_bonus`
(gravel's 10% flint), `apply_bonus`/`ore_drops` and `explosion_decay` — a stone-only fixture would say
nothing about whether an ore's bonus functions no-op correctly at fortune 0.

Predicted-and-observed drops under the empty context: `stone`→`cobblestone`×1, `dirt`→`dirt`×1,
`coal_ore`→`coal`×1, `iron_ore`→`raw_iron`×1 (each across 64 seeds), `gravel`→`flint` or `gravel` with the
flint share bracketed to `0.07..0.13` over 4,096 samples, which excludes both `table_bonus` never passing and
always passing.

With a tool (#539), each value derived from the jar's own record and not from the roller:

| block × tool | predicted | how it is asserted |
|---|---|---|
| Silk Touch on `stone` / `gravel` / `coal_ore` / `iron_ore` | the block itself × 1 | exact, every seed of 256; the processed item must **never** appear |
| plain pickaxe on `stone` | `cobblestone` × 1 | exact, 64 seeds — the row that separates a real `ItemPredicate` from tool-presence |
| bare hand on `stone` | **nothing** | absence, with the dirt row and the pickaxe row as its controls |
| Fortune 3 (and 4) on `gravel` | `flint`, **every** seed | `chances[min(level, 3)] = 1.0`, so certain — degenerate but exact, and level 4 also pins the clamp |
| Fortune 0/1/2 on `gravel` | flint share `0.1` / `0.142857` / `0.25` | ±0.03 over 8,192 samples (≈6σ), each number the table's own |
| Fortune 1/2/3 on `coal_ore` | support exactly `1..=level+1`, `P(1) = 2/(level+2)` | ceiling and full support asserted exactly; `P(1)` required to land nearer the clamped hypothesis than the unclamped one, both computed from outside constants |
| unenchanted tool on `coal_ore` | `coal` × 1 **and byte-identical placement to a bare hand** | the draw-count assertion; `ore_drops` guards on `level > 0` |
| Fortune 1 on `coal_ore` | placement **differs** from a bare hand | the other half — if it did not, `apply_bonus` never ran |

`loot.rs`'s own tests cover the two formulas no bundled table uses yet, from synthetic JSON whose expected
values come from the record: `uniform_bonus_count` (support exactly `1..=1 + M·L`, and one draw at level 0)
and `binomial_with_bonus_count` (mean `1 + (L + extra)·p`, at `extra = 3, p = 0.5714286` — the corpus's only
instantiation).

## Configuration

* `block_drops::BLOCK_DROPS_BEHAVIOR_SEED` — the per-connection roll/placement seed. Separate from
  `COMPOSTER_BEHAVIOR_SEED` so a composter click cannot shift which drop a later break rolls.
* `assets/loot_table/` — the bundled corpus (823 KB, 1,230 files), embedded by `build.rs` and parsed once per
  process behind a `OnceLock`. Measured in release: `load_bundled` plus a roll of all 1,230 tables completes
  inside a test that reports `0.01s`, so the first block break of a session carries no visible hitch.

## Dependencies

`loot.rs` (tables), `mobs.rs` (`MobSim`/`MobHandle`, item entities and their tick), `inventory.rs`
(destination and menu-slot mapping), `mob_spawn::SpawnRng` (draws), `lodestone-entity`'s `item_entity`
(lifecycle, motion, vanilla constants), `lodestone-model` (vocabulary). Names no packet id and no protocol
version.
